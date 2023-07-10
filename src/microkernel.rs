use wasmtime::component::*;
use wasmtime::{Config, Engine, Store};
use tokio::sync::mpsc;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use tokio::task::JoinError;
use tokio::task::JoinHandle;
use serde_json::json;

use crate::types::*;

bindgen!({
    path: "wit",
    world: "microkernel-process",
    async: true
});
const PROCESS_CHANNEL_CAPACITY: usize = 100;

struct Process {
    our_name: String,
    process_name: String,
    file_path: String,  // TODO: for use in restarting erroring process, ala midori
    send_to_loop: CardSender,
    send_to_process: mpsc::Sender<String>,
    send_to_terminal: PrintSender
}
struct ProcessAndHandle {
    process: Process,
    handle: JoinHandle<Result<(), JoinError>>  // TODO: for use in restarting erroring process, ala midori
}

type Processes = HashMap<String, ProcessAndHandle>;

#[async_trait::async_trait]
impl MicrokernelProcessImports for Process {
    async fn to_event_loop(
        &mut self,
        target_server: String,
        to: String,
        data_string: String
    ) -> Result<(), wasmtime::Error> {
        let data: serde_json::Value = serde_json::from_str(&data_string).expect(
            format!("given data not JSON string: {}", data_string).as_str()
        );

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
}

fn make_process_loop(
    process: Process,
    mut recv_in_process: mpsc::Receiver<String>,
    engine: &Engine
) -> Pin<Box<dyn Future<Output = Result<(), JoinError>> + Send>> {
    let component = match Component::from_file(&engine, &process.file_path) {
        Ok(result) => result,
        Err(error) => panic!("{}", error)
    };

    let mut linker = Linker::new(&engine);
    MicrokernelProcess::add_to_linker(&mut linker, |state: &mut Process| state).unwrap();

    let our_name = process.our_name.clone();
    let process_name = process.process_name.clone();
    let send_to_terminal = process.send_to_terminal.clone();
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
    fn new(
        our_name: String,
        process_name: String,
        file_path: &str,
        send_to_loop: CardSender,
        send_to_terminal: PrintSender,
        engine: &Engine,
    ) -> Self {

        let (send_to_process, recv_in_process) =
            mpsc::channel::<String>(PROCESS_CHANNEL_CAPACITY);

        let process =  Process {
                our_name: our_name.clone(),
                process_name: process_name.clone(),
                file_path: file_path.to_string(),
                send_to_loop: send_to_loop.clone(),
                send_to_process: send_to_process.clone(),
                send_to_terminal: send_to_terminal.clone()
            };

        let process_handle = tokio::spawn(
            make_process_loop(
                process,
                recv_in_process,
                engine
            )
        );

        ProcessAndHandle {
            process: Process {
                our_name: our_name,
                process_name: process_name,
                file_path: file_path.to_string(),
                send_to_loop,
                send_to_process,
                send_to_terminal: send_to_terminal.clone()
            },
            handle: process_handle,
        }
    }
}

fn json_to_string(json: &serde_json::Value) -> String {
    json.to_string().trim().trim_matches('"').to_string()
}

fn make_event_loop(
    our_name: String,
    processes: Processes,
    mut recv_in_loop: CardReceiver,
    send_to_wss: CardSender,
    send_to_terminal: PrintSender
) -> Pin<Box<dyn Future<Output = Result<(), JoinError>> + Send>> {
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
                            let _result = value
                                .process
                                .send_to_process
                                .send(data)
                                .await;
                            send_to_terminal
                                .send("event loop: sent to process".to_string())
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
        )
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
        )
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
