use anyhow::{anyhow, Result};
use ethers::types::H256;
use wasmtime::component::*;
use wasmtime::{Config, Engine, Store};
use tokio::sync::mpsc;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use tokio::task::JoinHandle;
use serde::{Serialize, Deserialize};

use crate::types::*;
//  WIT errors when `use`ing interface unless we import this and implement Host for Process below
use crate::microkernel::component::microkernel_process::types::Host;
use crate::microkernel::component::microkernel_process::types::WitWire;

bindgen!({
    path: "wit",
    world: "microkernel-process",
    async: true,
});
const PROCESS_CHANNEL_CAPACITY: usize = 100;
const PROCESS_MANAGER_CHANNEL_CAPACITY: usize = 10;

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
    is_long_running_process: bool,
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
    is_long_running_process: bool,
    // wasm_bytes: Vec<u8>,     // TODO: for use in faster/cached restarting?
}

impl Clone for ProcessMetadata {
    fn clone(&self) -> ProcessMetadata {
        ProcessMetadata {
            our_name: self.our_name.clone(),
            process_name: self.process_name.clone(),
            wasm_bytes_uri: self.wasm_bytes_uri.clone(),
            is_long_running_process: self.is_long_running_process.clone(),
        }
    }
}

struct Process {
    metadata: ProcessMetadata,
    state: serde_json::Value,
    send_to_terminal: PrintSender,
}

//  lives in process manager
type ProcesseMetadatas = HashMap<String, ProcessMetadata>;
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
    async fn modify_state(
        &mut self,
        json_pointer: String,
        new_value_string: String
    ) -> Result<String> {
        // let mut process_data = self.lock().await;
        let json =
            // process_data.state.pointer_mut(json_pointer.as_str()).ok_or(
            self.state.pointer_mut(json_pointer.as_str()).ok_or(
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
        // let process_data = self.lock().await;
        let json =
            // process_data.state.pointer(json_pointer.as_str()).ok_or(
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
        // let mut process_data = self.lock().await;
        // process_data.state = json;
        self.state = json;
        Ok(json_string)
    }

    async fn print_to_terminal(&mut self, message: String) -> Result<()> {
        // let process_data = self.lock().await;
        // process_data
        //     .send_to_terminal
        self.send_to_terminal
            .send(message)
            .await
            .expect("print_to_terminal: error sending");
        Ok(())
    }
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

async fn send_process_results_to_loop(
    results: Vec<(WitMessageTypeWithTarget, WitPayload)>,
    source_ship: String,
    source_app: String,
    is_expecting_response: bool,
    stack_len: usize,
    message_stack: MessageStack,
    send_to_loop: MessageSender,
) -> () {
    for (wit_message_type_with_target, wit_payload) in results.iter() {
        let mut message_stack = message_stack.clone();
        let (target_ship, target_app, message_type) =
            match wit_message_type_with_target {
                WitMessageTypeWithTarget::Request(type_with_target) => {
                    //  set target based on request type
                    (
                        type_with_target.target_ship.clone(),
                        type_with_target.target_app.clone(),
                        MessageType::Request(
                            type_with_target.is_expecting_response
                        ),
                    )
                },
                WitMessageTypeWithTarget::Response => {
                    //  if at chain start & dont want response, continue;
                    //   else target is most recent message source
                    if !is_expecting_response & (1 >= stack_len) {
                        continue;
                    } else {
                        (
                            message_stack[stack_len - 1]
                                .wire.
                                source_ship
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
            json: match wit_payload.json {
                Some(ref json_string) => serde_json::from_str(&json_string).unwrap(),
                None => None,
            },
            bytes: wit_payload.bytes.clone(),
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

async fn make_process_loop(
    metadata: ProcessMetadata,
    send_to_loop: MessageSender,
    send_to_terminal: PrintSender,
    mut recv_in_process: MessageReceiver,
    wasm_bytes: Vec<u8>,
    engine: &Engine,
) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
    let (our_name, process_name, is_long_running_process) =
        (
            metadata.our_name.clone(),
            metadata.process_name.clone(),
            metadata.is_long_running_process.clone()
        );

    let component = Component::new(&engine, &wasm_bytes)
        .expect("make_process_loop: couldn't read file");

    let mut linker = Linker::new(&engine);
    MicrokernelProcess::add_to_linker(&mut linker, |state: &mut Process| state).unwrap();
    // std::mem::drop(process_data);  //  unlock

    let mut store = Store::new(
        engine,
        Process {
            metadata,
            state: serde_json::Value::Null,
            send_to_terminal: send_to_terminal.clone(),
        },
    );

    Box::pin(
        async move {
            let (bindings, _) = MicrokernelProcess::instantiate_async(
                &mut store,
                &component,
                &linker
            ).await.unwrap();
            let results: Vec<WitMessage> = bindings
                .call_init(
                    &mut store,
                    &our_name.clone(),
                    &process_name.clone(),
                ).await.unwrap();
            for result in &results {
                let result = Message {
                    message_type: match result.message_type {
                        WitMessageType::Request(is_expecting_response) => MessageType::Request(is_expecting_response),
                        WitMessageType::Response => MessageType::Response,
                    },
                    wire: Wire {
                        source_ship: result.wire.source_ship.clone(),
                        source_app: result.wire.source_app.clone(),
                        target_ship: result.wire.target_ship.clone(),
                        target_app: result.wire.target_app.clone(),
                    },
                    payload: Payload {
                        json: match result.payload.json {
                            Some(ref json_string) => serde_json::from_str(&json_string).unwrap(),
                            None => None,
                        },
                        bytes: result.payload.bytes.clone(),
                    },
                };
                send_to_loop
                    .send(vec![result])
                    .await
                    .unwrap();
            }
            let mut i = 0;
            loop {
                let mut input_message_stack = recv_in_process
                    .recv()
                    .await
                    .unwrap();

                println!("{}: got message_stack: {:?}", process_name, input_message_stack);

                send_to_terminal
                    .send(format!("{}: got message_stack: [", process_name))
                    .await
                    .unwrap();
                for m in input_message_stack.iter() {
                    send_to_terminal.send(format!("    {}", m)).await.unwrap();
                }
                send_to_terminal
                    .send("]".to_string())
                    .await
                    .unwrap();

                //  for return
                let stack_len = input_message_stack.len();
                let (source_ship, source_app, message_type) = (
                    input_message_stack[stack_len-1].wire.target_ship.clone(),
                    input_message_stack[stack_len-1].wire.target_app.clone(),
                    input_message_stack[stack_len-1].message_type.clone(),
                );

                let input_wit_message_stack =
                    convert_message_stack_to_wit_message_stack(&input_message_stack).await;
                let input_wit_message_stack: Vec<&WitMessage> =
                    input_wit_message_stack.iter().collect();

                match message_type {
                    MessageType::Request(is_expecting_response) => {
                        let results: Vec<(WitMessageTypeWithTarget, WitPayload)> = 
                            bindings.call_run_write(
                                &mut store,
                                input_wit_message_stack.as_slice(),
                            ).await.unwrap();
                        if !is_expecting_response & (1 < stack_len) {
                            //  pop off requests that dont expect responses
                            //   UNLESS they are the request that started the chain
                            input_message_stack.pop();
                        }
                        send_process_results_to_loop(
                            results,
                            source_ship,
                            source_app,
                            is_expecting_response,
                            stack_len,
                            input_message_stack,
                            send_to_loop.clone(),
                        ).await;
                    },
                    MessageType::Response => {
                        let results: Vec<(WitMessageTypeWithTarget, WitPayload)> = 
                            bindings.call_handle_response(
                                &mut store,
                                input_wit_message_stack.as_slice(),
                            ).await.unwrap();
                        //  pop message_stack twice: once for response message,
                        //   and once for request message
                        //   and Then place new message on stack
                        input_message_stack.pop();
                        input_message_stack.pop();
                        send_process_results_to_loop(
                            results,
                            source_ship,
                            source_app,
                            false,
                            stack_len,
                            input_message_stack,
                            send_to_loop.clone(),
                        ).await;
                    },
                };
                i = i + 1;
                send_to_terminal
                    .send(format!("{}: ran process step {}", process_name, i))
                    .await
                    .unwrap();
                if !is_long_running_process {
                    break;
                }
            }
            Ok(())
        }
    )
}

async fn send_process_manager_results_to_loop(
    our_name: String,
    process_name: String,
    is_expecting_response: bool,
    stack_len: usize,
    mut message_stack: MessageStack,
    send_to_loop: MessageSender,
) -> () {
    let json_payload = serde_json::to_value(
        KernelRequest::StopProcess(KernelStopProcess { process_name,})
    ).unwrap();
    let kernel_stop_process_message = Message {
        message_type: MessageType::Request(is_expecting_response),
        wire: Wire {
            source_ship: our_name.clone(),
            source_app: "process_manager".to_string(),
            target_ship: our_name.clone(),
            target_app: "kernel".to_string(),
        },
        payload: Payload {
            json: Some(json_payload),
            bytes: None,
        },
    };
    if !is_expecting_response & (1 < stack_len) {
        //  pop off requests that dont expect responses
        //   UNLESS they are the request that started the chain
        message_stack.pop();
    }
    message_stack.push(kernel_stop_process_message);
    send_to_loop.send(message_stack).await.unwrap();
}

//  TODO: remove unnecessary println!s;
//        change necessary to send_to_terminals
async fn make_process_manager_loop(
    our_name: String,
    send_to_loop: MessageSender,
    send_to_terminal: PrintSender,
    mut recv_in_process_manager: MessageReceiver,
) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
    Box::pin(
        async move {
            let reserved_process_names = vec![
                "filesystem".to_string(),
                "process_manager".to_string(),
                "terminal".to_string(),
                "http_server".to_string(),
            ];
            let reserved_process_names: HashSet<String> = reserved_process_names
                .into_iter()
                .collect();
            let mut metadatas: ProcesseMetadatas = HashMap::new();
            loop {
                let mut message_stack = recv_in_process_manager.recv().await.unwrap();
                let stack_len = message_stack.len();
                let message = message_stack[stack_len - 1].clone();
                send_to_terminal
                    .send("process manager: called with stack: [".to_string())
                    .await
                    .unwrap();
                for m in message_stack.iter() {
                    send_to_terminal.send(format!("    {}", m)).await.unwrap();
                }
                send_to_terminal
                    .send("]".to_string())
                    .await
                    .unwrap();
                //  TODO: validate source/target?
                let Some(value) = message.payload.json else {
                    send_to_terminal
                        .send(
                            format!(
                                "process manager: got payload with no json source, target: {:?}, {:?} {:?} {:?}",
                                message.wire.source_ship,
                                message.wire.source_app,
                                message.wire.target_ship,
                                message.wire.target_app,
                            )
                        )
                        .await
                        .unwrap();
                    continue;
                };
                match message.message_type {
                    MessageType::Request(is_expecting_response) => {
                        let process_manager_command: ProcessManagerCommand =
                            serde_json::from_value(value)
                            .expect("process manager: could not parse to command");
                        match process_manager_command {
                            ProcessManagerCommand::Start(start) => {
                                println!("process manager: start");
                                if reserved_process_names.contains(&start.process_name) {
                                    println!(
                                        "process manager: cannot add process {} with name amongst {:?}",
                                        &start.process_name,
                                        reserved_process_names.iter().collect::<Vec<_>>(),
                                    );
                                    continue;
                                }

                                let get_bytes_message = Message {
                                    message_type: MessageType::Request(true),
                                    wire: Wire {
                                        source_ship: our_name.clone(),
                                        source_app: "process_manager".to_string(),
                                        //  TODO: target should be inferred from process.data.file_uri 
                                        target_ship: our_name.clone(),
                                        target_app: "filesystem".to_string(),
                                    },
                                    payload: Payload {
                                        json: Some(
                                            serde_json::to_value(
                                                FileSystemCommand {
                                                    uri_string: start.wasm_bytes_uri.clone(),
                                                    command: FileSystemAction::Read,
                                                }
                                            ).unwrap()
                                        ),
                                        bytes: None,
                                    },
                                };
                                if !is_expecting_response & (1 < stack_len) {
                                    //  pop off requests that dont expect responses
                                    //   UNLESS they are the request that started the chain
                                    message_stack.pop();
                                }
                                message_stack.push(get_bytes_message);
                                send_to_loop.send(message_stack).await.unwrap();
                            },
                            ProcessManagerCommand::Stop(stop) => {
                                //  TODO: refactor Stop and Restart since 99% of code is shared
                                println!("process manager: stop");
                                let _ = metadatas
                                    .remove(&stop.process_name)
                                    .unwrap();

                                send_process_manager_results_to_loop(
                                    our_name.clone(),
                                    stop.process_name,
                                    false,
                                    stack_len,
                                    message_stack,
                                    send_to_loop.clone(),
                                ).await;

                                println!("process manager: {:?}", metadatas.keys().collect::<Vec<_>>());
                            },
                            ProcessManagerCommand::Restart(restart) => {
                                //  TODO: refactor Stop and Restart since 99% of code is shared
                                println!("process manager: restart");

                                send_process_manager_results_to_loop(
                                    our_name.clone(),
                                    restart.process_name,
                                    true,
                                    stack_len,
                                    message_stack,
                                    send_to_loop.clone(),
                                ).await;
                            },
                        }
                    },
                    MessageType::Response => {
                        match (
                            message.wire.source_ship,
                            message.wire.source_app.as_str(),
                            message.payload.bytes,
                        ) {
                            (
                                our_name,
                                "filesystem",
                                Some(wasm_bytes),
                            ) => {
                                //  get process_name out of stack
                                let start_message = message_stack[stack_len - 3]
                                    .clone();
                                let Some(value) = start_message.payload.json else {
                                    //  TODO: handle error or bail?
                                    println!("process_manager: couldnt access start message from stack while handling filesystem reponse; stack: {:?}", message_stack);
                                    continue;
                                };
                                let ProcessManagerCommand::Start(start) =
                                    serde_json::from_value(value).unwrap() else {
                                    //  TODO: handle error or bail?
                                    println!("process_manager: couldnt parse start message from stack while handling filesystem reponse; stack: {:?}", message_stack);
                                    continue;
                                };

                                let json_payload = serde_json::to_value(
                                    KernelRequest::StartProcess(ProcessStart {
                                        process_name: start.process_name.clone(),
                                        wasm_bytes_uri: start.wasm_bytes_uri.clone(),
                                        is_long_running_process: start
                                            .is_long_running_process
                                            .clone(),
                                    })
                                ).unwrap();
                                let kernel_start_process_message = Message {
                                    message_type: MessageType::Request(true),
                                    wire: Wire {
                                        source_ship: our_name.clone(),
                                        source_app: "process_manager".to_string(),
                                        target_ship: our_name.clone(),
                                        target_app: "kernel".to_string(),
                                    },
                                    payload: Payload {
                                        json: Some(json_payload),
                                        bytes: Some(wasm_bytes),
                                    },
                                };
                                message_stack.pop();
                                message_stack.pop();
                                message_stack.push(kernel_start_process_message);
                                send_to_loop.send(message_stack).await.unwrap();
                            },
                            (
                                our_name,
                                "kernel",
                                None,
                            ) => {
                                match serde_json::from_value(value) {
                                    Ok(KernelResponse::StartProcess(metadata)) => {
                                        metadatas.insert(
                                            metadata.process_name.clone(),
                                            metadata,
                                        );
                                        //  TODO: response?
                                        continue;
                                    },
                                    Ok(KernelResponse::StopProcess(_stop)) => {
                                        //  if in response to a Restart, send new Start
                                        let restart_message = message_stack[stack_len - 3]
                                            .clone();
                                        let Some(value) = restart_message.payload.json else {
                                            //  TODO: handle error or bail?
                                            println!("process_manager: couldnt access restart message from stack while handling filesystem reponse; stack: {:?}", message_stack);
                                            continue;
                                        };
                                        let ProcessManagerCommand::Restart(restart) =
                                            serde_json::from_value(value).unwrap() else {
                                            //  TODO: handle error or bail?
                                            println!("process_manager: couldnt parse restart message from stack while handling filesystem reponse; stack: {:?}", message_stack);
                                            continue;
                                        };

                                        let removed = metadatas
                                            .remove(&restart.process_name)
                                            .unwrap();

                                        let json_payload = serde_json::to_value(
                                            ProcessManagerCommand::Start(ProcessStart {
                                                process_name: removed.process_name,
                                                wasm_bytes_uri: removed.wasm_bytes_uri,
                                                is_long_running_process: removed
                                                    .is_long_running_process,
                                            })
                                        ).unwrap();
                                        let kernel_start_process_message = Message {
                                            message_type: MessageType::Request(true),
                                            wire: Wire {
                                                source_ship: our_name.clone(),
                                                source_app: "process_manager".to_string(),
                                                target_ship: our_name.clone(),
                                                target_app: "process_manager".to_string(),
                                            },
                                            payload: Payload {
                                                json: Some(json_payload),
                                                bytes: None,
                                            },
                                        };
                                        message_stack.pop();
                                        message_stack.pop();
                                        message_stack.push(kernel_start_process_message);
                                        send_to_loop.send(message_stack).await.unwrap();
                                        continue;
                                    },
                                    Err(e) => {
                                        println!("process_manager: kernel response unexpected case; error: {} stack: {:?}", e, message_stack);
                                        continue;
                                    },
                                }
                            },
                            _ => {
                                //  TODO: handle error or bail?
                                println!("process_manager: response unexpected case; stack: {:?}", message_stack);
                                continue;
                            },
                        }
                    },
                }
            }
        }
    )
}


async fn handle_kernel_request(
    our_name: String,
    stack_len: usize,
    mut message_stack: MessageStack,
    message: Message,
    send_to_loop: MessageSender,
    send_to_process_manager: MessageSender,
    send_to_terminal: PrintSender,
    senders: &mut Senders,
    process_handles: &mut ProcessHandles,
    engine: &Engine,
) {
    let Some(value) = message.payload.json else {
        send_to_terminal
            .send(
                format!(
                    "kernel: got kernel command with no json source, stack: {:?}",
                    message_stack,
                )
            )
            .await
            .unwrap();
        return;
    };
    let kernel_request: KernelRequest =
        serde_json::from_value(value)
        .expect("kernel: could not parse to command");
    match kernel_request {
        KernelRequest::StartProcess(cmd) => {
            let Some(wasm_bytes) = message.payload.bytes else {
                send_to_terminal
                    .send(
                        format!(
                            "kernel: StartProcess requires bytes; stack: {:?}",
                            message_stack,
                        )
                    )
                    .await
                    .unwrap();
                return;
            };
            let (send_to_process, recv_in_process) =
                mpsc::channel::<MessageStack>(PROCESS_CHANNEL_CAPACITY);
            senders.insert(cmd.process_name.clone(), send_to_process);
            let metadata = ProcessMetadata {
                our_name: our_name.to_string(),
                process_name: cmd.process_name.clone(),
                wasm_bytes_uri: cmd.wasm_bytes_uri.clone(),
                is_long_running_process: cmd.is_long_running_process.clone(),
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
            send_to_process_manager
                .send(message_stack)
                .await
                .unwrap();
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
            send_to_process_manager
                .send(message_stack)
                .await
                .unwrap();
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
    send_to_http: MessageSender,
    send_to_process_manager: MessageSender,
    send_to_terminal: PrintSender,
    engine: Engine,
) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
    Box::pin(
        async move {
            let mut senders: Senders = HashMap::new();
            senders.insert("filesystem".to_string(), send_to_fs);
            senders.insert("process_manager".to_string(), send_to_process_manager.clone());
            senders.insert("http_server".to_string(), send_to_http.clone());

            let mut process_handles: ProcessHandles = HashMap::new();
            loop {
                let message_stack = recv_in_loop.recv().await.unwrap();
                let stack_len = message_stack.len();
                let message = message_stack[stack_len - 1].clone();
                send_to_terminal
                    .send(
                        format!(
                            "event loop: got json message: source, target, payload.json: {:?} {:?}, {:?} {:?}, {:?}",
                            message.wire.source_ship,
                            message.wire.source_app,
                            message.wire.target_ship,
                            message.wire.target_app,
                            message.payload.json,
                        )
                    )
                    .await
                    .unwrap();
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
                            send_to_process_manager.clone(),
                            send_to_terminal.clone(),
                            &mut senders,
                            &mut process_handles,
                            &engine,
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
    send_to_loop: MessageSender,
    send_to_terminal: PrintSender,
    recv_in_loop: MessageReceiver,
    send_to_wss: MessageSender,
    send_to_fs: MessageSender,
    send_to_http: MessageSender,
) {
    let mut config = Config::new();
    config.async_support(true);
    config.wasm_component_model(true);
    let engine = Engine::new(&config).unwrap();

    let (send_to_process_manager, recv_in_process_manager) =
        mpsc::channel::<MessageStack>(PROCESS_MANAGER_CHANNEL_CAPACITY);

    let event_loop_handle = tokio::spawn(
        make_event_loop(
            our.name.clone(),
            recv_in_loop,
            send_to_loop.clone(),
            send_to_wss,
            send_to_fs,
            send_to_http,
            send_to_process_manager,
            send_to_terminal.clone(),
            engine,
        ).await
    );

    let process_manager_handle = tokio::spawn(
        make_process_manager_loop(
            our.name.clone(),
            send_to_loop,
            send_to_terminal,
            recv_in_process_manager,
        ).await
    );

    _ = tokio::join!(
        event_loop_handle,
        process_manager_handle,
    );
}
