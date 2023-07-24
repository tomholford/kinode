use anyhow::Result;
use wasmtime::component::*;
use wasmtime::{Config, Engine, Store};
use tokio::sync::mpsc;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use tokio::task::JoinHandle;
use tokio::fs;
use serde::{Serialize, Deserialize};

use crate::types::*;
//  WIT errors when `use`ing interface unless we import this and implement Host for Process below
use crate::microkernel::component::microkernel_process::types::Host;
use crate::microkernel::component::microkernel_process::types::WitMessageType;
use crate::microkernel::component::microkernel_process::types::WitPayload;
use crate::microkernel::component::microkernel_process::types::WitProtomessageType;
use crate::microkernel::component::microkernel_process::types::WitWire;

bindgen!({
    path: "wit",
    world: "microkernel-process",
    async: true,
});
const PROCESS_CHANNEL_CAPACITY: usize = 100;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum ProcessManagerCommand {
    Start(ProcessStart),
    Stop(ProcessManagerStop),
    Restart(ProcessManagerRestart),
}
#[derive(Debug, Serialize, Deserialize)]
struct ProcessStart {
    process_name: String,
    wasm_bytes_uri: String,
}
#[derive(Debug, Serialize, Deserialize)]
struct ProcessManagerStop {
    process_name: String,
}
#[derive(Debug, Serialize, Deserialize)]
struct ProcessManagerRestart {
    process_name: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum KernelRequest {
    StartProcess(ProcessStart),
    StopProcess(KernelStopProcess),
}
#[derive(Debug, Serialize, Deserialize)]
struct KernelStopProcess {
    process_name: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum KernelResponse {
    StartProcess(ProcessMetadata),
    StopProcess(KernelStopProcess),
}

#[derive(Debug, Serialize, Deserialize)]
struct ProcessMetadata {
    our_name: String,
    process_name: String,
    wasm_bytes_uri: String,  // TODO: for use in restarting erroring process, ala midori
    // wasm_bytes: Vec<u8>,     // TODO: for use in faster/cached restarting?
}

impl Clone for ProcessMetadata {
    fn clone(&self) -> ProcessMetadata {
        ProcessMetadata {
            our_name: self.our_name.clone(),
            process_name: self.process_name.clone(),
            wasm_bytes_uri: self.wasm_bytes_uri.clone(),
        }
    }
}

struct Process {
    metadata: ProcessMetadata,
    recv_in_process: MessageReceiver,
    send_to_loop: MessageSender,
    send_to_terminal: PrintSender,
    current_message_stack: MessageStack,
}

//  live in event loop
type Senders = HashMap<String, MessageSender>;
type ProcessHandles = HashMap<String, JoinHandle<Result<()>>>;

fn json_to_string(json: &serde_json::Value) -> String {
    json.to_string().trim().trim_matches('"').to_string()
}

impl Host for Process {
}

#[async_trait::async_trait]
impl MicrokernelProcessImports for Process {
    async fn yield_results(&mut self, results: Vec<WitProtomessage>) -> Result<()> {
        send_process_results_to_loop(
            results,
            &mut self.current_message_stack,
            self.metadata.our_name.clone(),
            self.metadata.process_name.clone(),
            self.send_to_loop.clone(),
        ).await;
        Ok(())
    }

    async fn await_next_message(&mut self) -> Result<Vec<WitMessage>> {
        let mut next_message_stack = self.recv_in_process.recv().await.unwrap();

        print_stack_to_terminal(
            format!(
                "{}: got message_stack",
                self.metadata.process_name,
            ).as_str(),
            &next_message_stack,
            self.send_to_terminal.clone(),
        ).await;

        let next_wit_message_stack =
            convert_message_stack_to_wit_message_stack(&next_message_stack).await;

        let stack_len = next_message_stack.len();
        self.current_message_stack = match next_message_stack[stack_len - 1].message_type.clone() {
            MessageType::Request(is_expecting_response) => {
                //  pop off requests that dont expect responses
                //   UNLESS they are the request that started the chain
                if !is_expecting_response & (1 < stack_len) {
                    next_message_stack.truncate(stack_len - 1);
                    next_message_stack
                } else {
                    next_message_stack
                }
            },
            MessageType::Response => {
                //  pop message_stack twice: once for response message,
                //   and once for request message
                next_message_stack.truncate(stack_len - 2);
                next_message_stack
            },
        };

        Ok(next_wit_message_stack)
    }

    async fn print_to_terminal(&mut self, message: String) -> Result<()> {
        self.send_to_terminal
            .send(message)
            .await
            .expect("print_to_terminal: error sending");
        Ok(())
    }
}

async fn send_process_results_to_loop(
    results: Vec<WitProtomessage>,
    message_stack: &mut MessageStack,
    source_ship: String,
    source_app: String,
    send_to_loop: MessageSender,
) -> () {
    let stack_len = message_stack.len();
    let is_expecting_response =
        if stack_len == 0 {
            false
        } else {
            match message_stack[stack_len - 1].message_type {
                MessageType::Request(is_expecting_response) => is_expecting_response,
                MessageType::Response => false,
            }
        };
    for WitProtomessage { protomessage_type, payload } in &results {
        let mut message_stack = message_stack.clone();
        let (target_ship, target_app, message_type) =
            match protomessage_type {
                WitProtomessageType::Request(type_with_target) => {
                    //  set target based on request type
                    (
                        type_with_target.target_ship.clone(),
                        type_with_target.target_app.clone(),
                        MessageType::Request(
                            type_with_target.is_expecting_response
                        ),
                    )
                },
                WitProtomessageType::Response => {
                    //  if at chain start & dont want response, continue;
                    //   else target is most recent message source
                    if !is_expecting_response & (1 >= stack_len) {
                        continue;
                    } else {
                        (
                            message_stack[stack_len - 1]
                                .wire
                                .source_ship
                                .clone(),
                            message_stack[stack_len - 1]
                                .wire
                                .source_app
                                .clone(),
                            MessageType::Response,
                        )
                    }
                },
            };
        let payload = Payload {
            json: match payload.json {
                Some(ref json_string) => serde_json::from_str(&json_string).unwrap(),
                None => None,
            },
            bytes: payload.bytes.clone(),
        };
        let message = Message {
            message_type,
            wire: Wire {
                source_ship: source_ship.clone(),
                source_app: source_app.clone(),
                target_ship,
                target_app,
            },
            payload,
        };

        message_stack.push(message);
        send_to_loop
            .send(message_stack)
            .await
            .unwrap();
    }
}

async fn print_stack_to_terminal(
    context_string: &str,
    message_stack: &MessageStack,
    send_to_terminal: PrintSender,
) {
    send_to_terminal
        .send(format!("{}: [", context_string))
        .await
        .unwrap();
    for m in message_stack {
        send_to_terminal.send(format!("    {}", m)).await.unwrap();
    }
    send_to_terminal
        .send("]".to_string())
        .await
        .unwrap();
}

async fn convert_message_stack_to_wit_message_stack(message_stack: &Vec<Message>) -> Vec<WitMessage> {
    message_stack.iter().map(|m: &Message| {
        let wit_payload = WitPayload {
            json: match m.payload.json.clone() {
                Some(value) => Some(json_to_string(&value)),
                None => None,
            },
            bytes: m.payload.bytes.clone(),
        };
        let wit_message_type = match m.message_type {
            MessageType::Request(is_expecting_response) => {
                WitMessageType::Request(is_expecting_response)
            },
            MessageType::Response => WitMessageType::Response,
        };
        WitMessage {
            message_type: wit_message_type,
            wire: WitWire {
                source_ship: m.wire.source_ship.clone(),
                source_app: m.wire.source_app.clone(),
                target_ship: m.wire.target_ship.clone(),
                target_app: m.wire.target_app.clone(),
            },
            payload: wit_payload,
        }
    }).collect()
}

async fn make_process_loop(
    metadata: ProcessMetadata,
    send_to_loop: MessageSender,
    send_to_terminal: PrintSender,
    recv_in_process: MessageReceiver,
    wasm_bytes: Vec<u8>,
    engine: &Engine,
) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
    let our_name = metadata.our_name.clone();
    let process_name = metadata.process_name.clone();

    let component = Component::new(&engine, &wasm_bytes)
        .expect("make_process_loop: couldn't read file");

    let mut linker = Linker::new(&engine);
    MicrokernelProcess::add_to_linker(&mut linker, |state: &mut Process| state).unwrap();

    let mut store = Store::new(
        engine,
        Process {
            metadata,
            recv_in_process,
            send_to_loop,
            send_to_terminal: send_to_terminal.clone(),
            current_message_stack: vec![],
        },
    );

    Box::pin(
        async move {
            let (bindings, _) = MicrokernelProcess::instantiate_async(
                &mut store,
                &component,
                &linker
            ).await.unwrap();

            //  process loop happens inside the WASM component process -- if desired
            let _ = bindings.call_run_process(
                &mut store,
                &our_name,
                &process_name,
            ).await;

            //  TODO: propagate error from process returning?
            Ok(())
        }
    )
}

async fn handle_kernel_request(
    our_name: String,
    stack_len: usize,
    mut message_stack: MessageStack,
    message: Message,
    send_to_loop: MessageSender,
    send_to_terminal: PrintSender,
    senders: &mut Senders,
    process_handles: &mut ProcessHandles,
    engine: &Engine,
) {
    let Some(value) = message.payload.json else {
        print_stack_to_terminal(
            "kernel: got kernel command with no json source, stack",
            &message_stack,
            send_to_terminal.clone(),
        ).await;
        return;
    };
    let kernel_request: KernelRequest =
        serde_json::from_value(value)
        .expect("kernel: could not parse to command");
    match kernel_request {
        KernelRequest::StartProcess(cmd) => {
            let Some(wasm_bytes) = message.payload.bytes else {
                print_stack_to_terminal(
                    "kernel: StartProcess requires bytes; stack",
                    &message_stack,
                    send_to_terminal.clone(),
                ).await;
                return;
            };
            let (send_to_process, recv_in_process) =
                mpsc::channel::<MessageStack>(PROCESS_CHANNEL_CAPACITY);
            senders.insert(cmd.process_name.clone(), send_to_process);
            let metadata = ProcessMetadata {
                our_name: our_name.to_string(),
                process_name: cmd.process_name.clone(),
                wasm_bytes_uri: cmd.wasm_bytes_uri.clone(),
            };
            process_handles.insert(
                cmd.process_name.clone(),
                tokio::spawn(
                    make_process_loop(
                        metadata.clone(),
                        send_to_loop.clone(),
                        send_to_terminal.clone(),
                        recv_in_process,
                        wasm_bytes,
                        engine,
                    ).await
                ),
            );

            let start_completed_message = Message {
                message_type: MessageType::Response,
                wire: Wire {
                    source_ship: our_name.clone(),
                    source_app: "kernel".to_string(),
                    target_ship: our_name.clone(),
                    target_app: "process_manager".to_string(),
                },
                payload: Payload {
                    json: Some(
                        serde_json::to_value(
                            KernelResponse::StartProcess(metadata)
                        ).unwrap()
                    ),
                    bytes: None,
                },
            };
            message_stack.push(start_completed_message);
            if let Some(send_to_process_manager) = senders.get("process_manager") {
                send_to_process_manager
                    .send(message_stack)
                    .await
                    .unwrap();
            }
            return;
        },
        KernelRequest::StopProcess(cmd) => {
            let _ = senders.remove(&cmd.process_name);
            let process_handle = process_handles
                .remove(&cmd.process_name).unwrap();
            process_handle.abort();
            let MessageType::Request(
                is_expecting_response
            ) = message.message_type else {
                println!("kernel: StopProcess unexpected response instead of request");
                return;
            };
            if !is_expecting_response & (1 < stack_len) {
                //  pop off requests that dont expect responses
                //   UNLESS they are the request that started the chain
                return;
            }
            let json_payload = serde_json::to_value(
                KernelResponse::StopProcess(KernelStopProcess {
                    process_name: cmd.process_name.clone(),
                })
            ).unwrap();
            let stop_completed_message = Message {
                message_type: MessageType::Response,
                wire: Wire {
                    source_ship: our_name.clone(),
                    source_app: "kernel".to_string(),
                    target_ship: our_name.clone(),
                    target_app: "process_manager".to_string(),
                },
                payload: Payload {
                    json: Some(json_payload),
                    bytes: None,
                },
            };
            message_stack.push(stop_completed_message);
            if let Some(send_to_process_manager) = senders.get("process_manager") {
                send_to_process_manager
                    .send(message_stack)
                    .await
                    .unwrap();
            }
            return;
        },
    }
}

async fn make_event_loop(
    our_name: String,
    mut recv_in_loop: MessageReceiver,
    send_to_loop: MessageSender,
    send_to_wss: MessageSender,
    send_to_fs: MessageSender,
    send_to_terminal: PrintSender,
    engine: Engine,
) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
    Box::pin(
        async move {
            let mut senders: Senders = HashMap::new();
            senders.insert("filesystem".to_string(), send_to_fs);
            let mut process_handles: ProcessHandles = HashMap::new();
            loop {
                let message_stack = recv_in_loop.recv().await.unwrap();
                let stack_len = message_stack.len();
                let message = message_stack[stack_len - 1].clone();
                // send_to_terminal
                //     .send(
                //         format!(
                //             "event loop: got json message: source, target, payload.json: {:?} {:?}, {:?} {:?}, {:?}",
                //             message.wire.source_ship,
                //             message.wire.source_app,
                //             message.wire.target_ship,
                //             message.wire.target_app,
                //             message.payload.json,
                //         )
                //     )
                //     .await
                //     .unwrap();
                if our_name != message.wire.target_ship {
                    match send_to_wss.send(message_stack).await {
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
                    let to = message.wire.target_app.clone();
                    if to == "kernel" {
                        handle_kernel_request(
                            our_name.clone(),
                            stack_len,
                            message_stack,
                            message,
                            send_to_loop.clone(),
                            send_to_terminal.clone(),
                            &mut senders,
                            &mut process_handles,
                            &engine,
                        ).await;
                    //  XX temporary branch to assist in pure networking debugging
                    } else if to == "ws" {
                        let _ = send_to_terminal.send(
                            format!(
                                "\x1b[3;32m{}: {}\x1b[0m",
                                message.wire.source_ship, message.payload.json.unwrap()
                            )
                        ).await;
                    } else {
                        //  pass message to appropriate runtime/process
                        match senders.get(&to) {
                            Some(sender) => {
                                let _result = sender
                                    .send(message_stack)
                                    .await;
                            }
                            None => {
                                send_to_terminal
                                    .send(
                                        format!(
                                            "event loop: don't have {} amongst registered processes: {:?}",
                                            to,
                                            senders.keys().collect::<Vec<_>>()
                                        )
                                    )
                                    .await
                                    .unwrap();
                            }
                        }
                    }
                }
            }
        }
    )
}


pub async fn kernel(
    our: &Identity,
    process_manager_wasm_path: String,
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

    let event_loop_handle = tokio::spawn(
        make_event_loop(
            our.name.clone(),
            recv_in_loop,
            send_to_loop.clone(),
            send_to_wss,
            send_to_fs,
            send_to_terminal.clone(),
            engine,
        ).await
    );

    let process_manager_wasm_bytes = fs::read(&process_manager_wasm_path).await.unwrap();
    let start_process_manager_message: MessageStack = vec![
        Message {
            message_type: MessageType::Request(false),
            wire: Wire {
                source_ship: our.name.clone(),
                source_app: "kernel".to_string(),
                target_ship: our.name.clone(),
                target_app: "kernel".to_string(),
            },
            payload: Payload {
                json: Some(serde_json::to_value(
                    KernelRequest::StartProcess(
                        ProcessStart{
                            process_name: "process_manager".to_string(),
                            wasm_bytes_uri: process_manager_wasm_path,
                        }
                    )
                ).unwrap()),
                bytes: Some(process_manager_wasm_bytes),
            },
        },
    ];
    send_to_loop.send(start_process_manager_message).await.unwrap();

    let _ = event_loop_handle.await;
}
