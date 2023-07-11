use anyhow::{anyhow, Result};
use wasmtime::component::*;
use wasmtime::{Config, Engine, Store};
use tokio::sync::{mpsc, Mutex, RwLock};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use tokio::task::JoinHandle;
use serde_json::json;
use std::sync::Arc;

use crate::types::*;
use crate::microkernel::component::microkernel_process::types::Host;

bindgen!({
    path: "wit",
    world: "microkernel-process",
    async: true,
});
const PROCESS_CHANNEL_CAPACITY: usize = 100;

struct ProcessData {
    our_name: String,
    process_name: String,
    file_uri: String,  // TODO: for use in restarting erroring process, ala midori
    file_bytes: Vec<u8>,  // TODO: for use in restarting erroring process, ala midori
    state: serde_json::Value,
    send_to_loop: MessageSender,
    // send_to_process: mpsc::Sender<String>,
    send_to_process: MessageSender,
    send_to_terminal: PrintSender
}
type Process = Arc<Mutex<ProcessData>>;
struct ProcessAndHandle {
    process: Process,
    handle: JoinHandle<Result<()>>  // TODO: for use in restarting erroring process, ala midori
}

type Processes = Arc<RwLock<HashMap<String, ProcessAndHandle>>>;

fn json_to_string(json: &serde_json::Value) -> String {
    json.to_string().trim().trim_matches('"').to_string()
}

impl Host for Process {
}

#[async_trait::async_trait]
impl MicrokernelProcessImports for Process {
    async fn to_event_loop(
        &mut self,
        target: WitAppNode,
        // data_string: String
        wit_payload: WitPayload,
    ) -> Result<()> {
        let payload: Payload = match wit_payload {
            WitPayload::Json(payload_string) => {
                Payload::Json(
                    serde_json::from_str(&payload_string).expect(
                        format!("given data not JSON string: {}", payload_string).as_str()
                    )
                )
            },
            WitPayload::Bytes(payload_bytes) => {
                Payload::Bytes(payload_bytes)
            },
        };
        // let data: serde_json::Value = serde_json::from_str(&data_string).expect(
        //     format!("given data not JSON string: {}", data_string).as_str()
        // );

        let process_data = self.lock().await;

        let message_json = json!({
            "source": AppNode {
                server: process_data.our_name.clone(),
                app: process_data.process_name.clone(),
            },
            "target": AppNode { 
                server: target.server,
                app: target.app,
            },
            // "payload": Payload::Json(data),
            "payload": payload,
        });
        let message: Message = serde_json::from_value(message_json).unwrap();

        process_data.send_to_loop.send(message).await.expect("to_event_loop: error sending");
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
        let process_data = self.lock().await;
        let json =
            process_data.state.pointer(json_pointer.as_str()).ok_or(
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

    async fn print_to_terminal(&mut self, message: String) -> Result<()> {
        let process_data = self.lock().await;
        process_data
            .send_to_terminal
            .send(message)
            .await
            .expect("print_to_terminal: error sending");
        Ok(())
    }
}

async fn make_process_loop(
    process: Process,
    // mut recv_in_process: mpsc::Receiver<String>,
    mut recv_in_process: MessageReceiver,
    engine: &Engine
) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
    println!("mpl: 0");
    let process_data = process.lock().await;
    let our_name = process_data.our_name.clone();
    let process_name = process_data.process_name.clone();
    let file_uri = process_data.file_uri.clone();
    let send_to_loop = process_data.send_to_loop.clone();
    let send_to_terminal = process_data.send_to_terminal.clone();
    std::mem::drop(process_data);  //  unlock

    println!("mpl: 1");
    let get_bytes_message = Message {
        source: AppNode {
            server: our_name.clone(),
            app: process_name.clone(),
        },
        //  TODO: target should be read from process.data.file_uri
        target: AppNode {
            server: our_name.clone(),
            app: "filesystem".to_string(),
        },
        payload: Payload::Json(
            json!({
                "uri": file_uri,
            })
        ),
    };
    println!("mpl: 2");
    send_to_loop.send(get_bytes_message).await.unwrap();
    println!("mpl: 3");
    let message_from_loop = recv_in_process.recv().await.unwrap();
    println!("mpl: 4");
    if "filesystem".to_string() != message_from_loop.target.app {
        panic!(
            "message_process_loop: expected bytes message from filesystem, not {:?}",
            message_from_loop,
        );
    }
    let Payload::Bytes(file_contents) = message_from_loop.payload else {
        panic!(
            "message_process_loop: expected bytes message from filesystem, not {:?}",
            message_from_loop,
        );
    };
    let mut process_data = process.lock().await;
    process_data.file_bytes = file_contents.clone();

    let component = Component::new(&engine, &file_contents)
        .expect("make_process_loop: couldn't read file");

    let mut linker = Linker::new(&engine);
    MicrokernelProcess::add_to_linker(&mut linker, |state: &mut Process| state).unwrap();
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
            bindings
                // .call_init(&mut store, &our_name)
                .call_init(
                    &mut store,
                    &WitAppNode {
                        server: our_name.clone(),
                        app: process_name.clone(),
                    })
                .await
                .unwrap();
            let mut i = 0;
            loop {
                let message_from_loop = recv_in_process
                    .recv()
                    .await
                    .unwrap();
                send_to_terminal
                    .send(
                        format!(
                            "{}: got message from loop: {:?}",
                            process_name,
                            message_from_loop,
                        )
                    )
                    .await
                    .unwrap();
                let wit_payload = match message_from_loop.payload {
                        Payload::Json(value) => WitPayload::Json(serde_json::to_string(&value).unwrap()),
                        Payload::Bytes(bytes) => WitPayload::Bytes(bytes),
                };
                let wit_message = WitMessage {
                    source: &WitAppNode {
                        server: message_from_loop.source.server,
                        app: message_from_loop.source.app,
                    },
                    target: &WitAppNode {
                        server: message_from_loop.target.server,
                        app: message_from_loop.target.app,
                    },
                    payload: &wit_payload,
                    // payload: match message_from_loop.payload {
                    //     Payload::Json(value) => &WitPayload::Json(serde_json::to_string(&value).unwrap()),
                    //     Payload::Bytes(bytes) => &WitPayload::Bytes(bytes),
                    // }
                };
                bindings
                    // .call_run_write(&mut store, &our_name, &message_from_loop)
                    .call_run_write(
                        &mut store,
                        wit_message,
                    )
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
        file_uri: &str,
        send_to_loop: MessageSender,
        send_to_terminal: PrintSender,
        engine: &Engine,
    ) -> Self {
        let (send_to_process, recv_in_process) =
            // mpsc::channel::<String>(PROCESS_CHANNEL_CAPACITY);
            mpsc::channel::<Message>(PROCESS_CHANNEL_CAPACITY);

        let process = Arc::new(Mutex::new(ProcessData {
                our_name: our_name,
                process_name: process_name,
                file_uri: file_uri.to_string(),
                file_bytes: Vec::new(),
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
    mut recv_in_loop: MessageReceiver,
    send_to_wss: MessageSender,
    send_to_fs: MessageSender,
    send_to_terminal: PrintSender
) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
    Box::pin(
        async move {
            println!("event loop: running");
            loop {
                let next_message = recv_in_loop.recv().await.unwrap();
                if let Payload::Json(_) = next_message.payload {
                    send_to_terminal
                        .send(format!("event loop: got json message: {:?}", next_message))
                        .await
                        .unwrap();
                } else {
                    send_to_terminal
                        .send(format!("event loop: got bytes message source, target: {:?}, {:?}", next_message.source, next_message.target))
                        .await
                        .unwrap();
                }
                if our_name != next_message.target.server {
                    match send_to_wss.send(next_message).await {
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
                    let to = next_message.target.app.clone();
                    if "filesystem".to_string() == to {
                        //  request bytes from filesystem
                        //  TODO: generalize this to arbitrary URIs
                        //        (e.g., fs, http, peer, ...)
                        let _ = send_to_fs
                            .send(next_message)
                            .await;
                        continue;
                    }
                    //  pass message to appropriate process
                    println!("el: 0");
                    let processes_lock = processes.read().await;
                    println!("el: 1");
                    match processes_lock.get(&to) {
                        Some(value) => {
                            println!("el: 2");
                            let process = value.process.lock().await;
                            println!("el: 3");
                            let _result = process
                                .send_to_process
                                .send(next_message)
                                .await;
                            println!("el: 3");
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
                                        processes_lock.keys().collect::<Vec<_>>()
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
    send_to_loop: MessageSender,
    send_to_terminal: PrintSender,
    recv_in_loop: MessageReceiver,
    send_to_wss: MessageSender,
    send_to_fs: MessageSender,
) {
    let mut config = Config::new();
    config.async_support(true);
    config.wasm_component_model(true);
    let engine = Engine::new(&config).unwrap();

    let mut processes: Processes = Arc::new(RwLock::new(HashMap::new()));

    println!("mk: 0");

    let event_loop_handle = tokio::spawn(
        make_event_loop(
            our_name.to_string(),
            Arc::clone(&processes),
            recv_in_loop,
            send_to_wss,
            send_to_fs,
            send_to_terminal.clone()
        )
    );

    println!("mk: 1");

    let mut processes_lock = processes.write().await;
    let process_name = "http_server".to_string();
    let file_uri = "fs://http_server.wasm";
    processes_lock.insert(
        process_name.clone(),
        ProcessAndHandle::new(
            our_name.to_string(),
            process_name.clone(),
            &file_uri,
            send_to_loop.clone(),
            send_to_terminal.clone(),
            &engine
        ).await
    );

    println!("mk: 2");

    let process_name = "hi_lus_lus".to_string();
    let file_uri = "fs://hi_lus_lus.wasm";
    processes_lock.insert(
        process_name.clone(),
        ProcessAndHandle::new(
            our_name.to_string(),
            process_name.clone(),
            &file_uri,
            send_to_loop.clone(),
            send_to_terminal.clone(),
            &engine
        ).await
    );

    println!("mk: 3");

    let process_name = "poast".to_string();
    let file_uri = "fs://poast.wasm";
    processes_lock.insert(
        process_name.clone(),
        ProcessAndHandle::new(
            our_name.to_string(),
            process_name.clone(),
            &file_uri,
            send_to_loop.clone(),
            send_to_terminal.clone(),
            &engine
        ).await
    );

    std::mem::drop(processes_lock);

    let _event_loop_result = event_loop_handle.await;
}
