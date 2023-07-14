use anyhow::{anyhow, Result};
use wasmtime::component::*;
use wasmtime::{Config, Engine, Store};
use tokio::sync::{mpsc, Mutex, RwLock};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use tokio::task::JoinHandle;
use std::sync::Arc;
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
    Start(ProcessManagerStart),
    Stop(ProcessManagerStop),
    Restart(ProcessManagerRestart),
}
#[derive(Debug, Serialize, Deserialize)]
struct ProcessManagerStart {
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

struct ProcessData {
    our_name: String,
    process_name: String,
    wasm_bytes_uri: String,  // TODO: for use in restarting erroring process, ala midori
    wasm_bytes: Vec<u8>,     // TODO: for use in restarting erroring process, ala midori
    state: serde_json::Value,
    send_to_loop: MessageSender,
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
    // async fn to_event_loop(
    //     &mut self,
    //     target_ship: String,
    //     target_app: String,
    //     message_type: WitMessageType,
    //     wit_payload: WitPayload,
    //     messages: MessageStack,
    // ) -> Result<()> {
    //     let payload = Payload {
    //         json: match wit_payload.json {
    //             Some(payload_string) => {
    //                 Some(
    //                     serde_json::from_str(&payload_string).expect(
    //                         format!("given data not JSON string: {}", payload_string).as_str()
    //                     )
    //                 )
    //             },
    //             None => None,
    //         },
    //         bytes: wit_payload.bytes,
    //     };

    //     let process_data = self.lock().await;

    //     let message = Message {
    //         wire: Wire {
    //             source_ship: process_data.our_name.clone(),
    //             source_app: process_data.process_name.clone(),
    //             target_ship: target_ship,
    //             target_app: target_app,
    //         },
    //         message_type: match message_type {
    //             WitMessageType::Request(is_expecting_response) => {
    //                 MessageType::Request(is_expecting_response)
    //             },
    //             WitMessageType::Response => MessageType::Response,
    //         },
    //         payload: payload,
    //     };
    //     messages.push(message);

    //     process_data.send_to_loop.send(messages).await.expect("to_event_loop: error sending");
    //     Ok(())
    // }

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

async fn init_process(process: Process) {
    let (our_name, process_name, wasm_bytes_uri, send_to_loop) = {
        let process_data = process.lock().await;
        (
            process_data.our_name.clone(),
            process_data.process_name.clone(),
            process_data.wasm_bytes_uri.clone(),
            process_data.send_to_loop.clone(),
        )
    };

    let get_bytes_message = Message {
        message_type: MessageType::Request(false),
        wire: Wire {
            source_ship: our_name.clone(),
            source_app: process_name.clone(),
            //  TODO: target should be inferred from process.data.file_uri 
            target_ship: our_name.clone(),
            target_app: "filesystem".to_string(),
        },
        payload: Payload{
            json: Some(
                serde_json::to_value(
                    FileSystemCommand{
                        uri_string: wasm_bytes_uri,
                        command: FileSystemAction::Read,
                    }
                ).unwrap()
            ),
            bytes: None,
        },
    };
    //  This is a fake solution, but this will be ripped out soon anyways
    let mut message_stack: Vec<Message> = Vec::new();
    message_stack.push(get_bytes_message);
    send_to_loop.send(message_stack).await.unwrap();
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

// async fn convert_wit_message_stack_to_message_stack(wit_message_stack: &Vec<WitMessage>) -> Vec<Message> {
//     wit_message_stack.iter().map(|m: &WitMessage| {
//         let payload = Payload {
//             json: match m.payload.json.clone() {
//                 Some(value) => Some(serde_json::from_str(&value)),
//                 None => None,
//             },
//             bytes: m.payload.bytes.clone(),
//         };
//         let message_type = match m.message_type {
//             WitMessageType::Request(is_expecting_response) => {
//                 MessageType::Request(is_expecting_response)
//             },
//             WitMessageType::Response => MessageType::Response,
//         };
//         Message {
//             message_type: message_type,
//             wire: Wire {
//                 source_ship: &m.wire.source_ship,
//                 source_app: &m.wire.source_app,
//                 target_ship: &m.wire.target_ship,
//                 target_app: &m.wire.target_app
//             },
//             payload: &payload,
//         }
//     });
// }

async fn make_process_loop(
    process: Process,
    mut recv_in_process: MessageReceiver,
    message_stack_from_loop: MessageStack,
    engine: &Engine
) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
    let (our_name, process_name, send_to_loop, send_to_terminal) = {
        let process_data = process.lock().await;
        (
            process_data.our_name.clone(),
            process_data.process_name.clone(),
            process_data.send_to_loop.clone(),
            process_data.send_to_terminal.clone(),
        )
    };

    let stack_len = message_stack_from_loop.len();
    let message_from_loop = message_stack_from_loop[stack_len - 1].clone();

    if "filesystem".to_string() != message_from_loop.wire.source_app {
        panic!(
            "message_process_loop: expected bytes message from filesystem, not {:?}",
            message_from_loop.wire.source_app,
        );
    }
    let Payload { json: None, bytes: Some(wasm_bytes) } = message_from_loop.payload else {
        panic!(
            "message_process_loop: expected pure bytes message from filesystem, not {:?}",
            message_from_loop,
        );
    };
    let mut process_data = process.lock().await;
    process_data.wasm_bytes = wasm_bytes.clone();

    let component = Component::new(&engine, &wasm_bytes)
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
                .call_init(
                    &mut store,
                    &our_name.clone(),
                    &process_name.clone(),
                ).await.unwrap();
            let mut i = 0;
            loop {
                let mut input_message_stack = recv_in_process
                    .recv()
                    .await
                    .unwrap();

                println!("{}: got message_stack: {:?}", process_name, input_message_stack);

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

                // let messages_from_loop = recv_in_process
                //     .recv()
                //     .await
                //     .unwrap();

                // let wit_payload = WitPayload {
                //     json: match message_from_loop.payload.json {
                //         Some(value) => Some(json_to_string(&value)),
                //         None => None,
                //     },
                //     bytes: message_from_loop.payload.bytes,
                // };
                // let wit_message_type = match message_from_loop.message_type {
                //     MessageType::Request(is_expecting_response) => {
                //         WitMessageType::Request(is_expecting_response)
                //     },
                //     MessageType::Response => WitMessageType::Response,
                // };
                // let wit_message = WitMessage {
                //     message_type: wit_message_type,
                //     wire: WitWire {
                //         source_ship: &message_from_loop.wire.source_ship,
                //         source_app: &message_from_loop.wire.source_app,
                //         target_ship: &message_from_loop.wire.target_ship,
                //         target_app: &message_from_loop.wire.target_app
                //     },
                //     payload: &wit_payload,
                // };
                // convert_wit_message_stack_to_message_stack(
                match message_type {
                    MessageType::Request(is_expecting_response) => {
                        //  this code is nearly the same as the Response case below:
                        //   TODO: refactor
                        let results: Vec<(WitMessageTypeWithTarget, WitPayload)> = 
                            bindings.call_run_write(
                                &mut store,
                                input_wit_message_stack.as_slice(),
                            ).await.unwrap();
                        if !is_expecting_response {
                            //  pop off caller from stack
                            input_message_stack.pop();
                        }
                        //  push to message_stack
                        for (wit_message_type_with_target, wit_payload) in results.iter() {
                            let mut input_message_stack = input_message_stack.clone();
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
                                        //  if popped off only message in stack, continue;
                                        //   else target is most recent message source
                                        if (1 == stack_len) & !is_expecting_response {
                                            continue;
                                        } else {
                                            (
                                                input_message_stack[stack_len - 1]
                                                    .wire.
                                                    source_ship
                                                    .clone(),
                                                input_message_stack[stack_len - 1]
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
                                message_type: message_type,
                                wire: Wire {
                                    source_ship: source_ship.clone(),
                                    source_app: source_app.clone(),
                                    target_ship: target_ship,
                                    target_app: target_app,
                                },
                                payload: payload,
                            };

                            // let output_message_stack = input_message_stack.clone();
                            // output_message_stack.push(message);
                            input_message_stack.push(message);
                            send_to_loop
                                .send(input_message_stack)
                                .await
                                .unwrap();
                        }
                    },
                    MessageType::Response => {
                        //  this code is nearly the same as the Request case above:
                        //   TODO: refactor
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
                        //  push to message_stack
                        for (wit_message_type_with_target, wit_payload) in results.iter() {
                            let mut input_message_stack = input_message_stack.clone();
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
                                        //  if stack length == 0, continue; else
                                        //  set target based on most recent message source
                                        let stack_len = input_message_stack.len();
                                        if 0 == stack_len {
                                            continue;
                                        } else {
                                            (
                                                input_message_stack[stack_len - 1]
                                                    .wire.
                                                    source_ship
                                                    .clone(),
                                                input_message_stack[stack_len - 1]
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
                                message_type: message_type,
                                wire: Wire {
                                    source_ship: source_ship.clone(),
                                    source_app: source_app.clone(),
                                    target_ship: target_ship,
                                    target_app: target_app,
                                },
                                payload: payload,
                            };

                            // let output_message_stack = input_message_stack.clone();
                            // output_message_stack.push(message);
                            // send_to_loop
                            //     .send(output_message_stack)
                            //     .await
                            //     .unwrap();
                            input_message_stack.push(message);
                            send_to_loop
                                .send(input_message_stack)
                                .await
                                .unwrap();
                        }
                    },
                };
                // send_to_loop
                //     .send(message_stack)
                //     .await
                //     .unwrap();
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
        wasm_bytes_uri: &str,
        send_to_process: MessageSender,
        send_to_loop: MessageSender,
        send_to_terminal: PrintSender,
    ) -> Self {
        let process = Arc::new(Mutex::new(ProcessData {
                our_name: our_name,
                process_name: process_name,
                wasm_bytes_uri: wasm_bytes_uri.to_string(),
                wasm_bytes: Vec::new(),
                state: serde_json::Value::Null,
                send_to_loop: send_to_loop,
                send_to_process: send_to_process,
                send_to_terminal: send_to_terminal
            }));

        init_process(Arc::clone(&process)).await;

        ProcessAndHandle {
            process: Arc::clone(&process),
            handle: tokio::spawn(async {Ok(())}),  //  dummy placeholder
        }
    }

    async fn start(
        mut process: Self,
        recv_in_process: MessageReceiver,
        bytes_message: MessageStack,
        engine: &Engine
    ) -> Self {
        let process_handle = tokio::spawn(
            make_process_loop(
                Arc::clone(&process.process),
                recv_in_process,
                bytes_message,
                engine,
            ).await
        );
        process.handle = process_handle;
        process
    }
}

fn make_event_loop(
    our_name: String,
    processes: Processes,
    mut recv_in_loop: MessageReceiver,
    send_to_wss: MessageSender,
    send_to_fs: MessageSender,
    send_to_process_manager: MessageSender,
    send_to_terminal: PrintSender,
) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
    Box::pin(
        async move {
            loop {
                let next_message_stack = recv_in_loop.recv().await.unwrap();
                let stack_len = next_message_stack.len();
                let next_message = next_message_stack[stack_len - 1].clone();
                send_to_terminal
                    .send(
                        format!(
                            "event loop: got json message: source, target, payload.json: {:?} {:?}, {:?} {:?}, {:?}",
                            next_message.wire.source_ship,
                            next_message.wire.source_app,
                            next_message.wire.target_ship,
                            next_message.wire.target_app,
                            next_message.payload.json,
                        )
                    )
                    .await
                    .unwrap();
                if our_name != next_message.wire.target_ship {
                    match send_to_wss.send(next_message_stack).await {
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
                    let to = next_message.wire.target_app.clone();
                    if "filesystem".to_string() == to {
                        //  request bytes from filesystem
                        //  TODO: generalize this to arbitrary URIs
                        //        (e.g., fs, http, peer, ...)
                        let _ = send_to_fs
                            .send(next_message_stack)
                            .await;
                        continue;
                    } else if "process_manager".to_string() == to {
                        let _ = send_to_process_manager
                            .send(next_message_stack)
                            .await;
                        continue;
                    }
                    //  pass message to appropriate process
                    let processes_lock = processes.read().await;
                    match processes_lock.get(&to) {
                        Some(value) => {
                            let process = value.process.lock().await;
                            let _result = process
                                .send_to_process
                                .send(next_message_stack)
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

//  TODO: remove unnecessary println!s;
//        change necessary to send_to_terminals
async fn make_process_manager_loop(
    our_name: String,
    processes: Processes,
    send_to_loop: MessageSender,
    send_to_terminal: PrintSender,
    mut recv_in_process_manager: MessageReceiver,
    engine: Engine,
) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
    Box::pin(
        async move {
            let disallowed_process_names = vec![
                "filesystem".to_string(),
                "process_manager".to_string(),
                "terminal".to_string(),
            ];
            let disallowed_process_names: HashSet<String> = disallowed_process_names
                .into_iter()
                .collect();
            loop {
                let mut next_message_stack = recv_in_process_manager.recv().await.unwrap();
                let stack_len = next_message_stack.len();
                let next_message = next_message_stack[stack_len - 1].clone();
                println!("process manager: got {:?}", next_message);
                //  TODO: validate source/target?
                let Some(value) = next_message.payload.json else {
                    send_to_terminal
                        .send(
                            format!(
                                "process manager: got payload with no json source, target: {:?} {:?}, {:?} {:?}",
                                next_message.wire.source_ship,
                                next_message.wire.source_app,
                                next_message.wire.target_ship,
                                next_message.wire.target_app,
                            )
                        )
                        .await
                        .unwrap();
                    continue;
                };
                let process_manager_command: ProcessManagerCommand =
                    serde_json::from_value(value)
                    .expect("process manager: could not parse to command");
                println!("process manager: parsed {:?}", process_manager_command);
                match process_manager_command {
                    ProcessManagerCommand::Start(start) => {
                        println!("process manager: start");
                        if disallowed_process_names.contains(&start.process_name) {
                            println!(
                                "process manager: cannot add process {} with name amongst {:?}",
                                &start.process_name,
                                disallowed_process_names.iter().collect::<Vec<_>>(),
                            );
                            continue;
                        }
                        let (send_to_process, mut recv_in_process) =
                            mpsc::channel::<MessageStack>(PROCESS_CHANNEL_CAPACITY);
                        {
                            let mut processes_write = processes.write().await;
                            processes_write.insert(
                                start.process_name.clone(),
                                ProcessAndHandle::new(
                                    our_name.to_string(),
                                    start.process_name.clone(),
                                    &start.wasm_bytes_uri,
                                    send_to_process,
                                    send_to_loop.clone(),
                                    send_to_terminal.clone(),
                                ).await
                            );
                        }

                        //  Divide operation into two steps to avoid blocking event loop,
                        //   which needs read access to processes HashMap.
                        let bytes_message = recv_in_process.recv().await.unwrap();
                        {
                            let mut processes_write = processes.write().await;
                            let process = processes_write
                                .remove(&start.process_name)
                                .unwrap();
                            let process = ProcessAndHandle::start(
                                process,
                                recv_in_process,
                                bytes_message,
                                &engine
                            ).await;
                            processes_write.insert(
                                start.process_name.clone(),
                                process,
                            );
                        }
                    },
                    ProcessManagerCommand::Stop(stop) => {
                        println!("process manager: stop");
                        let mut processes_write = processes.write().await;
                        let removed = processes_write
                            .remove(&stop.process_name)
                            .unwrap();
                        removed.handle.abort();
                        println!("process manager: {:?}", processes_write.keys().collect::<Vec<_>>());
                    },
                    ProcessManagerCommand::Restart(restart) => {
                        println!("process manager: restart");
                        let mut processes_write = processes.write().await;
                        let removed = processes_write
                            .remove(&restart.process_name)
                            .unwrap();
                        removed.handle.abort();

                        let removed_process = removed.process.lock().await;
                        let restart_payload_json =
                            serde_json::to_value(
                                ProcessManagerCommand::Start(ProcessManagerStart {
                                    process_name: removed_process.process_name.clone(),
                                    wasm_bytes_uri: removed_process.wasm_bytes_uri.clone(),
                                })
                            ).unwrap();
                        let restart_message = Message {
                            message_type: MessageType::Request(true),
                            wire: Wire {
                                source_ship: our_name.clone(),
                                source_app: "process_manager".to_string(),
                                target_ship: removed_process.our_name.clone(),
                                target_app: "process_manager".to_string(),
                            },
                            payload: Payload {
                                json: Some(restart_payload_json),
                                bytes: None,
                            },
                        };
                        next_message_stack.push(restart_message);
                        let _ = send_to_loop.send(next_message_stack).await;
                    },
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

    let processes: Processes = Arc::new(RwLock::new(HashMap::new()));
    let (send_to_process_manager, recv_in_process_manager) =
        mpsc::channel::<MessageStack>(PROCESS_MANAGER_CHANNEL_CAPACITY);

    let event_loop_handle = tokio::spawn(
        make_event_loop(
            our_name.to_string(),
            Arc::clone(&processes),
            recv_in_loop,
            send_to_wss,
            send_to_fs,
            send_to_process_manager,
            send_to_terminal.clone(),
        )
    );

    let process_manager_handle = tokio::spawn(
        make_process_manager_loop(
            our_name.to_string(),
            Arc::clone(&processes),
            send_to_loop.clone(),
            send_to_terminal.clone(),
            recv_in_process_manager,
            engine,
        ).await
    );

    _ = tokio::join!(
        event_loop_handle,
        process_manager_handle,
    );
}
