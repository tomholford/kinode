use anyhow::Result;
use wasmtime::component::*;
use wasmtime::{Config, Engine, Store};
use tokio::sync::mpsc;
use std::collections::{HashMap, VecDeque};
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
    prompting_message: Option<WrappedMessage>,
    contexts: HashMap<u64, ProcessContext>,
    message_queue: VecDeque<WrappedMessage>,
}

#[derive(Clone, Debug)]
struct CauseMetadata {
    id: u64,
    process_node: ProcessNode,
    rsvp: Rsvp,
    message_type: MessageType,
}

#[derive(Debug)]
struct ProcessContext {
    proximate: CauseMetadata,         //  kernel only: for routing responses  TODO: needed?
    ultimate: Option<CauseMetadata>,  //  kernel only: for routing responses
    context: serde_json::Value,       //  input/output from process
}

impl CauseMetadata {
    fn new(wrapped_message: &WrappedMessage) -> Self {
        CauseMetadata {
            id: wrapped_message.id.clone(),
            process_node: ProcessNode {
                node: wrapped_message.message.wire.source_ship.clone(),
                process: wrapped_message.message.wire.source_app.clone(),
            },
            rsvp: wrapped_message.rsvp.clone(),
            message_type: wrapped_message.message.message_type.clone(),
        }
    }
}

impl ProcessContext {
    fn new(
        proximate: &WrappedMessage,
        ultimate: Option<&WrappedMessage>,
        context: Option<serde_json::Value>,
    ) -> Self {
        ProcessContext {
            proximate: CauseMetadata::new(proximate),
            ultimate: match ultimate {
                Some(ultimate) => Some(CauseMetadata::new(ultimate)),
                None => None,
            },
            context: match context {
                Some(c) => c,
                None => serde_json::Value::Null,
            },
        }
    }
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
    async fn yield_results(&mut self, results: Vec<(WitProtomessage, String)>) -> Result<()> {
        let _ = send_process_results_to_loop(
            results,
            self.metadata.our_name.clone(),
            self.metadata.process_name.clone(),
            self.send_to_loop.clone(),
            &self.prompting_message,
            &mut self.contexts,
        ).await;
        Ok(())
    }

    async fn await_next_message(&mut self) -> Result<(WitMessage, String)> {
        let (wrapped_message, process_input) = get_and_send_loop_message_to_process(
            &mut self.message_queue,
            &mut self.recv_in_process,
            &mut self.send_to_terminal,
            &self.contexts,
        ).await;
        self.prompting_message = Some(wrapped_message);
        process_input
    }

    async fn yield_and_await_response(&mut self, result: (WitProtomessage, String)) -> Result<(WitMessage, String)> {
        let ids = send_process_results_to_loop(
            vec![result],
            self.metadata.our_name.clone(),
            self.metadata.process_name.clone(),
            self.send_to_loop.clone(),
            &self.prompting_message,
            &mut self.contexts,
        ).await;

        if 1 != ids.len() {
            panic!("yield_and_await_response: must receive only 1 id back");
        }

        let (wrapped_message, process_input) = get_and_send_specific_loop_message_to_process(
            ids[0],
            &mut self.message_queue,
            &mut self.recv_in_process,
            &mut self.send_to_terminal,
            &self.contexts,
        ).await;
        self.prompting_message = Some(wrapped_message);
        process_input
    }

    async fn print_to_terminal(&mut self, message: String) -> Result<()> {
        self.send_to_terminal
            .send(Printout { verbosity: 1, content: message })
            .await
            .expect("print_to_terminal: error sending");
        Ok(())
    }

    async fn get_current_unix_time(&mut self) -> Result<u64> {
        Ok(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        )
    }

    async fn get_insecure_uniform_u64(&mut self) -> Result<u64> {
        Ok(rand::random())
    }
}

async fn get_and_send_specific_loop_message_to_process(
    awaited_message_id: u64,
    message_queue: &mut VecDeque<WrappedMessage>,
    recv_in_process: &mut MessageReceiver,
    send_to_terminal: &mut PrintSender,
    contexts: &HashMap<u64, ProcessContext>,
) -> (WrappedMessage, Result<(WitMessage, String)>) {
    loop {
        let wrapped_message = recv_in_process.recv().await.unwrap();
        //  if message id matches the one we sent out
        //   AND the message is not a websocket ack
        if (awaited_message_id == wrapped_message.id)
           & !(("ws" == wrapped_message.message.wire.source_app)
               & (Some(serde_json::Value::String("Success".into())) == wrapped_message.message.payload.json)
              ) {
            return send_loop_message_to_process(
                wrapped_message,
                send_to_terminal,
                contexts,
            ).await;
        }

        message_queue.push_back(wrapped_message);
    }
}

async fn get_and_send_loop_message_to_process(
    message_queue: &mut VecDeque<WrappedMessage>,
    recv_in_process: &mut MessageReceiver,
    send_to_terminal: &mut PrintSender,
    contexts: &HashMap<u64, ProcessContext>,
) -> (WrappedMessage, Result<(WitMessage, String)>) {
    let wrapped_message = recv_in_process.recv().await.unwrap();
    let wrapped_message =
        match message_queue.pop_front() {
            Some(m) => {
                message_queue.push_back(wrapped_message);
                m
            },
            None => wrapped_message,
        };

    send_loop_message_to_process(
        wrapped_message,
        send_to_terminal,
        contexts,
    ).await
}

async fn send_loop_message_to_process(
    wrapped_message: WrappedMessage,
    send_to_terminal: &mut PrintSender,
    contexts: &HashMap<u64, ProcessContext>,
) -> (WrappedMessage, Result<(WitMessage, String)>) {
    // print_stack_to_terminal(
    //     format!(
    //         "{}: got message_stack",
    //         self.metadata.process_name,
    //     ).as_str(),
    //     &next_message_stack,
    //     self.send_to_terminal.clone(),
    // ).await;

    let wit_message = convert_message_to_wit_message(&wrapped_message.message).await;

    let message_id = wrapped_message.id.clone();
    let message_type = wrapped_message.message.message_type.clone();
    (
        wrapped_message,
        match message_type {
            MessageType::Request(_) => Ok((wit_message, "".to_string())),
            MessageType::Response => {
                match contexts.get(&message_id) {
                    Some(ref context) => {
                        Ok((wit_message, serde_json::to_string(&context.context).unwrap()))
                    },
                    None => {
                        send_to_terminal
                            .send(Printout { verbosity: 1, content: "awm: couldn't find context for Response".into() })
                            .await
                            .unwrap();
                        Ok((wit_message, "".to_string()))
                    },
                }
            },
        },
    )
}

async fn send_process_results_to_loop(
    results: Vec<(WitProtomessage, String)>,
    source_ship: String,
    source_app: String,
    send_to_loop: MessageSender,
    prompting_message: &Option<WrappedMessage>,
    contexts: &mut HashMap<u64, ProcessContext>,
) -> Vec<u64> {
    let mut ids: Vec<u64> = Vec::new();
    for (WitProtomessage { protomessage_type, payload }, new_context_string) in &results {
        let new_context = match serde_json::from_str(new_context_string) {
            Ok(r) => Some(r),
            Err(_) => None,
        };
        let (id, rsvp, target_ship, target_app, message_type) =
            match protomessage_type {
                WitProtomessageType::Request(type_with_target) => {
                    let (id, rsvp) =
                        if type_with_target.is_expecting_response {
                            (rand::random(), None)
                        } else {
                            //  rsvp is set if there was a Request expecting Response
                            //   followed by Request(s) not expecting Response;
                            //   could also be None if entire chain of Requests are
                            //   not expecting Response
                            match prompting_message {
                                Some(ref prompting_message) => {
                                    match prompting_message.message.message_type {
                                        MessageType::Request(prompting_message_is_expecting_response) => {
                                            if prompting_message_is_expecting_response {
                                                (
                                                    prompting_message.id.clone(), //  TODO: need to reference count?
                                                    Some(ProcessNode {
                                                        node: prompting_message.message.wire.source_ship.clone(),
                                                        process: prompting_message.message.wire.source_app.clone(),
                                                    }),
                                                )
                                            } else {
                                                (
                                                    prompting_message.id.clone(),  //  TODO: need to reference count?
                                                    prompting_message.rsvp.clone(),
                                                )
                                            }
                                        },
                                        MessageType::Response => {
                                            (rand::random(), None)
                                            // panic!("oops: {:?}\n{}\n{}\n{}", results, source_ship, source_app, prompting_message)
                                        },
                                    }
                                },
                                None => {
                                    (rand::random(), None)
                                },
                            }
                        };
                    (
                        id,
                        rsvp,
                        type_with_target.target_ship.clone(),
                        type_with_target.target_app.clone(),
                        MessageType::Request(type_with_target.is_expecting_response),
                    )
                },
                WitProtomessageType::Response => {
                    let Some(ref prompting_message) = prompting_message else {
                        println!("need non-None prompting_message to handle Response");
                        continue;
                    };
                    let (id, target_ship, target_app) =
                        match prompting_message.message.message_type {
                            MessageType::Request(is_expecting_response) => {
                                if is_expecting_response {
                                    (
                                        prompting_message.id.clone(),
                                        prompting_message.message.wire.source_ship.clone(),
                                        prompting_message.message.wire.source_app.clone(),
                                    )
                                } else {
                                    let Some(rsvp) = prompting_message.rsvp.clone() else {
                                        println!("no rsvp set for response (prompting)");
                                        continue;
                                    };

                                    (
                                        prompting_message.id.clone(),
                                        rsvp.node.clone(),
                                        rsvp.process.clone(),
                                    )
                                }
                            },
                            MessageType::Response => {
                                let Some(context) = contexts.get(&prompting_message.id) else {
                                    println!("couldn't find context to route response");
                                    continue;
                                };
                                println!("sprtl: resp to resp; prox, ult: {:?}, {:?}", context.proximate, context.ultimate);
                                let Some(ref ultimate) = context.ultimate else {
                                    println!("couldn't find ultimate cause to route response");
                                    continue;
                                };

                                match ultimate.message_type {
                                    MessageType::Request(is_expecting_response) => {
                                        if is_expecting_response {
                                            (
                                                ultimate.id.clone(),
                                                ultimate.process_node.node.clone(),
                                                ultimate.process_node.process.clone(),
                                            )
                                        } else {
                                            let Some(rsvp) = ultimate.rsvp.clone() else {
                                                println!("no rsvp set for response (ultimate)");
                                                continue;
                                            };
                                            (
                                                ultimate.id.clone(),
                                                rsvp.node.clone(),
                                                rsvp.process.clone(),
                                            )
                                        }
                                    },
                                    MessageType::Response => {
                                        println!("ultimate as response unexpected case");
                                        continue;
                                    },
                                }
                            },
                        };
                    (
                        id,
                        None,
                        target_ship,
                        target_app,
                        MessageType::Response,
                    )
                },
            };

        let payload = Payload {
            json: match payload.json {
                Some(ref json_string) => serde_json::from_str(&json_string).unwrap_or(None),
                None => None,
            },
            bytes: payload.bytes.clone(),
        };
        let wrapped_message = WrappedMessage {
            id: id.clone(),
            rsvp,
            message: Message {
                message_type,
                wire: Wire {
                    source_ship: source_ship.clone(),
                    source_app: source_app.clone(),
                    target_ship,
                    target_app,
                },
                payload,
            }
        };

        // println!("contexts before modification");
        // for (key, val) in contexts.iter() {
        //     println!("{}: {:?}", key, val);
        // }

        //  modify contexts if necessary
        //   note that this could be rolled into the `match` making Message above;
        //   if performance is bad here, roll into above
        match wrapped_message.message.message_type {
            MessageType::Request(_) => {
                //  add context, as appropriate
                match prompting_message {
                    Some(ref prompting_message) => {
                        match prompting_message.message.message_type {
                            MessageType::Request(_prompting_message_is_expecting_response) => {
                                //  case: prompting_message_is_expecting_response
                                //   ultimate stored for source
                                //  case: !prompting_message_is_expecting_response
                                //   ultimate stored for rsvp
                                contexts.insert(
                                    wrapped_message.id,
                                    ProcessContext::new(
                                        &wrapped_message,
                                        Some(&prompting_message),
                                        new_context,
                                    )
                                );
                            },
                            MessageType::Response => {
                                match contexts.get(&prompting_message.id) {
                                    Some(context) => {
                                        //  ultimate is the ultimate of the prompt of Response
                                        contexts.insert(
                                            wrapped_message.id,
                                            ProcessContext {
                                                proximate: CauseMetadata::new(&wrapped_message),
                                                ultimate: context.ultimate.clone(),
                                                context: match new_context {
                                                    Some(new_context) => new_context,
                                                    None => serde_json::Value::Null,
                                                },
                                            },
                                        );
                                    },
                                    None => {
                                        //  should this even be allowed?
                                        contexts.insert(
                                            wrapped_message.id,
                                            ProcessContext::new(
                                                &wrapped_message,
                                                Some(&prompting_message),
                                                new_context,
                                            )
                                        );
                                    },
                                }
                            },
                        }
                    },
                    None => {
                        contexts.insert(
                            wrapped_message.id,
                            ProcessContext::new(
                                &wrapped_message,
                                None,
                                new_context,
                            )
                        );
                    },
                }
            },
            MessageType::Response => {
                //  clean up context we just used
                contexts.remove(&wrapped_message.id);
            },
        }

        // println!("contexts after modification");
        // for (key, val) in contexts.iter() {
        //     println!("{}: {:?}", key, val);
        // }

        send_to_loop
            .send(wrapped_message)
            .await
            .unwrap();

        ids.push(id);
    }
    ids
}

async fn convert_message_to_wit_message(m: &Message) -> WitMessage {
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
            send_to_loop: send_to_loop.clone(),
            send_to_terminal: send_to_terminal.clone(),
            prompting_message: None,
            contexts: HashMap::new(),
            message_queue: VecDeque::new(),
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
            match bindings.call_run_process(
                &mut store,
                &our_name,
                &process_name,
            ).await {
                Ok(()) => {},
                Err(e) => {
                    let _ = send_to_terminal
                        .send(Printout {
                            verbosity: 0,
                            content: format!(
                                "mk: process {} ended with error: {:?}",
                                process_name,
                                e,
                            ),
                        })
                        .await;
                }
            };

            //  clean up process metadata & channels
            send_to_loop
                .send(WrappedMessage {
                    id: rand::random(),
                    rsvp: None,
                    message: Message {
                        message_type: MessageType::Request(false),
                        wire: Wire {
                            source_ship: our_name.clone(),
                            source_app: "kernel".into(),
                            target_ship: our_name.clone(),
                            target_app: "process_manager".into(),
                        },
                        payload: Payload {
                            json: Some(serde_json::json!({
                                "type": "Stop",
                                "process_name": process_name,
                            })),
                            bytes: None,
                        },
                    },
                }
                )
                .await
                .unwrap();
            Ok(())
        }
    )
}

async fn handle_kernel_request(
    our_name: String,
    wrapped_message: WrappedMessage,
    send_to_loop: MessageSender,
    send_to_terminal: PrintSender,
    senders: &mut Senders,
    process_handles: &mut ProcessHandles,
    engine: &Engine,
) {
    let message = wrapped_message.message;
    let Some(value) = message.payload.json else {
        // print_stack_to_terminal(
        //     "kernel: got kernel command with no json source, stack",
        //     &message_stack,
        //     send_to_terminal.clone(),
        // ).await;
        return;
    };
    let kernel_request: KernelRequest =
        serde_json::from_value(value)
        .expect("kernel: could not parse to command");
    match kernel_request {
        KernelRequest::StartProcess(cmd) => {
            let Some(wasm_bytes) = message.payload.bytes else {
                // print_stack_to_terminal(
                //     "kernel: StartProcess requires bytes; stack",
                //     &message_stack,
                //     send_to_terminal.clone(),
                // ).await;
                return;
            };
            let (send_to_process, recv_in_process) =
                mpsc::channel::<WrappedMessage>(PROCESS_CHANNEL_CAPACITY);
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

            let start_completed_message = WrappedMessage {
                id: wrapped_message.id,
                rsvp: None,
                message: Message {
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
                }
            };
            if let Some(send_to_process_manager) = senders.get("process_manager") {
                send_to_process_manager
                    .send(start_completed_message)
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
                send_to_terminal
                    .send(Printout {
                        verbosity: 1,
                        content: "kernel: StopProcess requires Request, got Response".into()
                    })
                    .await
                    .unwrap();
                return;
            };
            if !is_expecting_response {
                return;
            }
            let json_payload = serde_json::to_value(
                KernelResponse::StopProcess(KernelStopProcess {
                    process_name: cmd.process_name.clone(),
                })
            ).unwrap();

            let stop_completed_message = WrappedMessage {
                id: wrapped_message.id,
                rsvp: None,
                message: Message {
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
                }
            };
            if let Some(send_to_process_manager) = senders.get("process_manager") {
                send_to_process_manager
                    .send(stop_completed_message)
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
    mut recv_debug_in_loop: DebugReceiver,
    send_to_loop: MessageSender,
    send_to_wss: MessageSender,
    send_to_fs: MessageSender,
    send_to_http_server: MessageSender,
    send_to_http_client: MessageSender,
    send_to_terminal: PrintSender,
    engine: Engine,
) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
    Box::pin(
        async move {
            let mut senders: Senders = HashMap::new();
            senders.insert("filesystem".to_string(), send_to_fs);
            senders.insert("http_server".to_string(), send_to_http_server.clone());
            senders.insert("http_client".to_string(), send_to_http_client);

            let mut process_handles: ProcessHandles = HashMap::new();
            let mut is_debug = false;
            loop {
                tokio::select! {
                    debug = recv_debug_in_loop.recv() => {
                        if let Some(DebugCommand::Toggle) = debug {
                            is_debug = !is_debug;
                        }
                    },
                    wrapped_message = recv_in_loop.recv() => {
                        while is_debug {
                            let debug = recv_debug_in_loop.recv().await.unwrap();
                            match debug {
                                DebugCommand::Toggle => is_debug = !is_debug,
                                DebugCommand::Step => break,
                            }
                        }

                        let Some(wrapped_message) = wrapped_message else {
                            send_to_terminal.send(Printout {
                                    verbosity: 1,
                                    content: "event loop: got None for message".to_string(),
                                }
                            ).await.unwrap();
                            continue;
                        };
                        // let wrapped_message = recv_in_loop.recv().await.unwrap();
                        send_to_terminal.send(
                            Printout {
                                verbosity: 1,
                                content: format!("event loop: got message: {}", wrapped_message)
                            }
                        ).await.unwrap();
                        // print_stack_to_terminal(
                        //     "event loop: got message stack",
                        //     &message_stack,
                        //     send_to_terminal.clone(),
                        // ).await;
                        if our_name != wrapped_message.message.wire.target_ship {
                            match send_to_wss.send(wrapped_message).await {
                                Ok(()) => {
                                    send_to_terminal
                                        .send(Printout {
                                            verbosity: 1,
                                            content: "event loop: sent to wss".to_string(),
                                        })
                                        .await
                                        .unwrap();
                                }
                                Err(e) => {
                                    send_to_terminal
                                        .send(Printout {
                                            verbosity: 1,
                                            content: format!("event loop: failed to send to wss: {}", e),
                                        })
                                        .await
                                        .unwrap();
                                }
                            }
                        } else {
                            let to = wrapped_message.message.wire.target_app.clone();
                            if to == "kernel" {
                                handle_kernel_request(
                                    our_name.clone(),
                                    wrapped_message,
                                    send_to_loop.clone(),
                                    send_to_terminal.clone(),
                                    &mut senders,
                                    &mut process_handles,
                                    &engine,
                                ).await;
                            //  XX temporary branch to assist in pure networking debugging
                            //  can be removed when ws WASM module is ready
                            } else if to == "ws" {
                                let _ = send_to_wss.send(wrapped_message).await;
                            } else {
                                //  pass message to appropriate runtime/process
                                match senders.get(&to) {
                                    Some(sender) => {
                                        let _result = sender
                                            .send(wrapped_message)
                                            .await;
                                    }
                                    None => {
                                        send_to_terminal
                                            .send(Printout {
                                                verbosity: 0,
                                                content: format!(
                                                    "event loop: don't have {} amongst registered processes: {:?}",
                                                    to,
                                                    senders.keys().collect::<Vec<_>>()
                                                )
                                            })
                                            .await
                                            .unwrap();
                                    }
                                }
                            }
                        }


                    },
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
    recv_debug_in_loop: DebugReceiver,
    send_to_wss: MessageSender,
    send_to_fs: MessageSender,
    send_to_http_server: MessageSender,
    send_to_http_client: MessageSender,
) {
    let mut config = Config::new();
    config.async_support(true);
    config.wasm_component_model(true);
    let engine = Engine::new(&config).unwrap();

    let event_loop_handle = tokio::spawn(
        make_event_loop(
            our.name.clone(),
            recv_in_loop,
            recv_debug_in_loop,
            send_to_loop.clone(),
            send_to_wss,
            send_to_fs,
            send_to_http_server,
            send_to_http_client,
            send_to_terminal.clone(),
            engine,
        ).await
    );

    // always start process manager on boot
    let process_manager_wasm_bytes = fs::read(&process_manager_wasm_path).await.unwrap();
    let start_process_manager_message = WrappedMessage {
        id: rand::random(),
        rsvp: None,
        message: Message {
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
    };
    send_to_loop.send(start_process_manager_message).await.unwrap();

    // always start terminal on boot
    let terminal_wasm_bytes = fs::read("terminal.wasm").await.unwrap();
    let start_terminal_message = WrappedMessage {
        id: rand::random(),
        rsvp: None,
        message: Message {
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
                            process_name: "terminal".into(),
                            wasm_bytes_uri: "terminal.wasm".into(),
                        }
                    )
                ).unwrap()),
                bytes: Some(terminal_wasm_bytes),
            },
        },
    };
    send_to_loop.send(start_terminal_message).await.unwrap();

    // always start http-bindings on boot
    let http_bindings_bytes = fs::read("http_bindings.wasm").await.unwrap();
    let start_http_bindings_message = WrappedMessage {
        id: rand::random(),
        rsvp: None,
        message: Message {
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
                            process_name: "http_bindings".into(),
                            wasm_bytes_uri: "http_bindings.wasm".into(),
                        }
                    )
                ).unwrap()),
                bytes: Some(http_bindings_bytes),
            },
        },
    };
    send_to_loop.send(start_http_bindings_message).await.unwrap();

    // always start apps-home on boot
    let apps_home_bytes = fs::read("apps_home.wasm").await.unwrap();
    let start_apps_home_message = WrappedMessage {
        id: rand::random(),
        rsvp: None,
        message: Message {
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
                            process_name: "apps_home".into(),
                            wasm_bytes_uri: "apps_home.wasm".into(),
                        }
                    )
                ).unwrap()),
                bytes: Some(apps_home_bytes),
            },
        },
    };
    send_to_loop.send(start_apps_home_message).await.unwrap();

    // DEMO ONLY: start file_transfer app at boot
    let ft_bytes = fs::read("file_transfer.wasm").await.unwrap();
    let start_apps_ft = WrappedMessage {
        id: rand::random(),
        rsvp: None,
        message: Message {
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
                            process_name: "file_transfer".into(),
                            wasm_bytes_uri: "file_transfer.wasm".into(),
                        }
                    )
                ).unwrap()),
                bytes: Some(ft_bytes),
            },
        },
    };
    send_to_loop.send(start_apps_ft).await.unwrap();

    let _ = event_loop_handle.await;
}
