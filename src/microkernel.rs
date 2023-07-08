use anyhow::{anyhow, Result};
use wasmtime::component::*;
use wasmtime::{Config, Engine, Store};
use tokio::sync::mpsc;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use tokio::task::JoinHandle;
use serde_json::json;
use std::sync::Arc;
use futures::lock::Mutex;
use futures::future::join_all;

use crate::types::*;

bindgen!({
    path: "wit",
    world: "microkernel-process",
    async: true
});
const PROCESS_CHANNEL_CAPACITY: usize = 100;

struct ProcessData {
    our_name: String,
    process_name: String,
    file_path: String,  // TODO: for use in restarting erroring process, ala midori
    state: serde_json::Value,
    send_to_loop: CardSender,
    send_to_process: mpsc::Sender<String>,
    send_to_terminal: PrintSender
}
type Process = Arc<Mutex<ProcessData>>;
struct ProcessAndHandle {
    process: Process,
    handle: JoinHandle<Result<()>>  // TODO: for use in restarting erroring process, ala midori
}

type Processes = HashMap<String, ProcessAndHandle>;

fn json_to_string(json: &serde_json::Value) -> String {
    json.to_string().trim().trim_matches('"').to_string()
}

#[async_trait::async_trait]
impl MicrokernelProcessImports for Process {
    async fn to_event_loop(
        &mut self,
        target_server: String,
        to: String,
        data_string: String
    ) -> Result<()> {
        let data: serde_json::Value = serde_json::from_str(&data_string).expect(
            format!("given data not JSON string: {}", data_string).as_str()
        );

        let self = self.lock().await;

        let message_json = json!({
            "source": self.our_name,
            "target": target_server,
            "payload": {
                "from": self.process_name,
                "to": to,
                "data": data
            }
        });
        let message: Card = serde_json::from_value(message_json).unwrap();

        self.send_to_loop.send(message).await.expect("to_event_loop: error sending");
        Ok(())
    }

    async fn modify_state(
        &mut self,
        json_pointer: String,
        new_value_string: String
    ) -> Result<String> {
        let mut process_data = self.lock().await;
        let json =
            process_data.state.pointer_mut(json_pointer.as_str()).ok_or(
                anyhow!(
                    format!(
                        "modify_state: state does not contain {:?}",
                        json_pointer
                    )
                )
            )?;
        let new_value = serde_json::from_str(&new_value_string)?;
        *json = new_value;
        Ok(new_value_string)
    }

    async fn fetch_state(&mut self, json_pointer: String) -> Result<String> {
        let self = self.lock().await;
        let json =
            self.state.pointer(json_pointer.as_str()).ok_or(
                anyhow!(
                    format!(
                        "fetch_state: state does not contain {:?}",
                        json_pointer
                    )
                )
            )?;
        Ok(json_to_string(json))
    }

    async fn set_state(&mut self, json_string: String) -> Result<String> {
        let json = serde_json::from_str(&json_string)?;
        let mut process_data = self.lock().await;
        process_data.state = json;
        Ok(json_string)
    }
}

async fn make_process_loop(
    process: Process,
    mut recv_in_process: mpsc::Receiver<String>,
    engine: &Engine
) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
    let process_data = process.lock().await;
    let component = Component::from_file(&engine, &process_data.file_path)
        .expect("make_process_loop: couldn't read file");

    let mut linker = Linker::new(&engine);
    MicrokernelProcess::add_to_linker(&mut linker, |state: &mut Process| state).unwrap();

    let our_name = process_data.our_name.clone();
    let process_name = process_data.process_name.clone();
    let send_to_terminal = process_data.send_to_terminal.clone();
    std::mem::drop(process_data);  //  unlock
    let mut store = Store::new(
        engine,
        process
    );

    Box::pin(
        async move {
            let (bindings, _) = MicrokernelProcess::instantiate_async(
                &mut store,
                &component,
                &linker
            ).await.unwrap();
            let mut i = 0;
            loop {
                let message_from_loop = recv_in_process
                    .recv()
                    .await
                    .unwrap();
                send_to_terminal
                    .send(
                        format!(
                            "{}: got message from loop: {}",
                            process_name,
                            message_from_loop
                        )
                    )
                    .await
                    .unwrap();
                bindings
                    .call_from_event_loop(&mut store, &our_name, &message_from_loop)
                    .await
                    .unwrap();
                i = i + 1;
                send_to_terminal
                    .send(format!("{}: ran process step {}", process_name, i))
                    .await
                    .unwrap();
            }
        }
    )
}

impl ProcessAndHandle {
    async fn new(
        our_name: String,
        process_name: String,
        file_path: &str,
        send_to_loop: CardSender,
        send_to_terminal: PrintSender,
        engine: &Engine,
    ) -> Self {
        let (send_to_process, recv_in_process) =
            mpsc::channel::<String>(PROCESS_CHANNEL_CAPACITY);

        let process = Arc::new(Mutex::new(ProcessData {
                our_name: our_name,
                process_name: process_name,
                file_path: file_path.to_string(),
                state: serde_json::Value::Null,
                send_to_loop: send_to_loop,
                send_to_process: send_to_process,
                send_to_terminal: send_to_terminal
            }));

        let process_loop =
            make_process_loop(
                Arc::clone(&process),
                recv_in_process,
                engine
            ).await;
        let process_handle = tokio::spawn(process_loop);

        ProcessAndHandle {
            process: Arc::clone(&process),
            handle: process_handle
        }
    }
}

fn make_event_loop(
    our_name: String,
    processes: Processes,
    mut recv_in_loop: CardReceiver,
    send_to_wss: CardSender,
    send_to_terminal: PrintSender
) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
    Box::pin(
        async move {
            loop {
                let next_card = recv_in_loop.recv().await.unwrap();
                send_to_terminal
                    .send(format!("event loop: got: {:?}", next_card))
                    .await
                    .unwrap();
                if our_name != next_card.target {
                    match send_to_wss.send(next_card).await {
                        Ok(()) => {
                            send_to_terminal
                                .send("event loop: sent to wss".to_string())
                                .await
                                .unwrap();
                        }
                        Err(e) => {
                            send_to_terminal
                                .send(format!("event loop: failed to send to wss: {}", e))
                                .await
                                .unwrap();
                        }
                    }
                } else {
                    let to = json_to_string(&next_card.payload["to"]);
                    let data = json_to_string(&next_card.payload["data"]);

                    match processes.get(&to) {
                        Some(value) => {
                            let process = value.process.lock().await;
                            let _result = process
                                .send_to_process
                                .send(data)
                                .await;
                            send_to_terminal
                                .send(
                                    format!(
                                        "event loop: sent to process; current state: {:?}",
                                        process.state,
                                    )
                                )
                                .await
                                .unwrap();
                        }
                        None => {
                            send_to_terminal
                                .send(
                                    format!(
                                        "event loop: don't have {} amongst registered processes: {:?}",
                                        to,
                                        processes.keys().collect::<Vec<_>>()
                                    )
                                )
                                .await
                                .unwrap();
                            continue;
                        }
                    }
                }
            }
        }
    )
}

pub async fn kernel(
    our_name: &str,
    send_to_loop: CardSender,
    send_to_terminal: PrintSender,
    recv_in_loop: CardReceiver,
    send_to_wss: CardSender
) {
    let mut config = Config::new();
    config.async_support(true);
    config.wasm_component_model(true);
    let engine = Engine::new(&config).unwrap();

    let mut processes: Processes = HashMap::new();

    let process_name = "http_server".to_string();
    let file_path = "./http_server.wasm";
    processes.insert(
        process_name.clone(),
        ProcessAndHandle::new(
            our_name.to_string(),
            process_name.clone(),
            &file_path,
            send_to_loop.clone(),
            send_to_terminal.clone(),
            &engine
        ).await
    );

    let process_name = "poast".to_string();
    let file_path = "./poast.wasm";
    processes.insert(
        process_name.clone(),
        ProcessAndHandle::new(
            our_name.to_string(),
            process_name.clone(),
            &file_path,
            send_to_loop.clone(),
            send_to_terminal.clone(),
            &engine
        ).await
    );

    let event_loop_handle = tokio::spawn(
        make_event_loop(
            our_name.to_string(),
            processes,
            recv_in_loop,
            send_to_wss,
            send_to_terminal.clone()
        )
    );

    let _event_loop_result = event_loop_handle.await;
}
