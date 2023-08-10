use anyhow::Result;
use wasmtime::component::*;
use wasmtime::{Config, Engine, Store, WasmBacktraceDetails};
use tokio::sync::mpsc;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use tokio::task::JoinHandle;
use tokio::fs;
use serde::{Serialize, Deserialize};

use wasmtime_wasi::preview2::{Table, WasiCtx, WasiCtxBuilder, WasiView, wasi};

use crate::types::*;
//  WIT errors when `use`ing interface unless we import this and implement Host for Process below
use crate::microkernel::component::microkernel_process::types::Host;
// use crate::microkernel::component::microkernel_process::types::WitErrorContent;
use crate::microkernel::component::microkernel_process::types::WitMessageContent;
use crate::microkernel::component::microkernel_process::types::WitMessageType;
use crate::microkernel::component::microkernel_process::types::WitProtomessageType;
use crate::microkernel::component::microkernel_process::types::WitRequestTypeWithTarget;

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
    our: ProcessNode,
    wasm_bytes_uri: String,  // TODO: for use in restarting erroring process, ala midori
}

impl Clone for ProcessMetadata {
    fn clone(&self) -> ProcessMetadata {
        ProcessMetadata {
            our: self.our.clone(),
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

struct ProcessWasi {
    process: Process,
    table: Table,
    wasi: WasiCtx,
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
        let Ok(ref message) = wrapped_message.message else { panic!("") };  //  TODO: need error?
        CauseMetadata {
            id: wrapped_message.id.clone(),
            process_node: message.source.clone(),
            rsvp: wrapped_message.rsvp.clone(),
            message_type: message.content.message_type.clone(),
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

// impl std::error::Error for WitError {
// }
// impl std::fmt::Display for WitError {
//     fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
//         write!(
//             f,
//             "WitError {{ foo: {}, bar: {} }}",
//             self.foo,
//             self.bar,
//         )
//     }
// }

//  live in event loop
type Senders = HashMap<String, MessageSender>;
type ProcessHandles = HashMap<String, JoinHandle<Result<()>>>;

impl Host for ProcessWasi {
}

impl WasiView for ProcessWasi {
    fn table(&self) -> &Table {
        &self.table
    }
    fn table_mut(&mut self) -> &mut Table {
        &mut self.table
    }
    fn ctx(&self) -> &WasiCtx {
        &self.wasi
    }
    fn ctx_mut(&mut self) -> &mut WasiCtx {
        &mut self.wasi
    }
}

#[async_trait::async_trait]
impl MicrokernelProcessImports for ProcessWasi {
    async fn yield_results(
        &mut self,
        results: Result<Vec<(WitProtomessage, String)>, WitUqbarErrorContent>,
    ) -> Result<()> {
        let _ = send_process_results_to_loop(
            results,
            self.process.metadata.our.clone(),
            // self.process.metadata.our_name.clone(),
            // self.process.metadata.process_name.clone(),
            self.process.send_to_loop.clone(),
            &self.process.prompting_message,
            &mut self.process.contexts,
        ).await;
        Ok(())
    }

    async fn await_next_message(&mut self) -> Result<Result<(WitMessage, String), WitUqbarError>> {
        let (wrapped_message, process_input) = get_and_send_loop_message_to_process(
            &mut self.process.message_queue,
            &mut self.process.recv_in_process,
            &mut self.process.send_to_terminal,
            &self.process.contexts,
        ).await;
        self.process.prompting_message = Some(wrapped_message);
        process_input
    }

    async fn yield_and_await_response(
        &mut self,
        target: WitProcessNode,
        payload: WitPayload,
    ) -> Result<Result<WitMessage, WitUqbarError>> {
        let protomessage = WitProtomessage {
            protomessage_type: WitProtomessageType::Request(WitRequestTypeWithTarget {
                is_expecting_response: true,
                target,
            }),
            payload,
        };

        let ids = send_process_results_to_loop(
            Ok(vec![(protomessage, "".into())]),
            self.process.metadata.our.clone(),
            self.process.send_to_loop.clone(),
            &self.process.prompting_message,
            &mut self.process.contexts,
        ).await;

        if 1 != ids.len() {
            panic!("yield_and_await_response: must receive only 1 id back");
        }

        let (wrapped_message, process_input) = get_and_send_specific_loop_message_to_process(
            ids[0],
            &mut self.process.message_queue,
            &mut self.process.recv_in_process,
            &mut self.process.send_to_terminal,
            &self.process.contexts,
        ).await;
        self.process.prompting_message = Some(wrapped_message);
        process_input
    }

    async fn print_to_terminal(&mut self, verbosity: u8, content: String) -> Result<()> {
        self.process.send_to_terminal
            .send(Printout { verbosity, content })
            .await
            .expect("print_to_terminal: error sending");
        Ok(())
    }

    async fn get_current_unix_time(&mut self) -> Result<u64> {
        get_current_unix_time()
    }

    async fn get_insecure_uniform_u64(&mut self) -> Result<u64> {
        Ok(rand::random())
    }
}

fn get_current_unix_time() -> Result<u64> {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(t) => Ok(t.as_secs()),
        Err(e) => Err(e.into()),
    }
}

fn json_to_string(json: &serde_json::Value) -> String {
    json.to_string().trim().trim_matches('"').to_string()
}

fn de_wit_process_node(wit: &WitProcessNode) -> ProcessNode {
    ProcessNode {
        node: wit.node.clone(),
        process: wit.process.clone(),
    }
}

fn en_wit_process_node(dewit: &ProcessNode) -> WitProcessNode {
    WitProcessNode {
        node: dewit.node.clone(),
        process: dewit.process.clone(),
    }
}

fn de_wit_error_content(wit: &WitUqbarErrorContent) -> UqbarErrorContent {
    UqbarErrorContent {
        kind: wit.kind.clone(),
        message: serde_json::from_str(&wit.message).unwrap(),  //  TODO: handle error?
        context: serde_json::from_str(&wit.context).unwrap(),  //  TODO: handle error?
    }
}

fn en_wit_error_content(dewit: &UqbarErrorContent, context: String) -> WitUqbarErrorContent {
    WitUqbarErrorContent {
        kind: dewit.kind.clone(),
        message: dewit.message.to_string(),
        context: if "" == context {
            dewit.context.to_string()
        } else {
            context
        }
    }
}

fn de_wit_error(wit: &WitUqbarError) -> UqbarError {
    UqbarError {
        source: ProcessNode {
            node: wit.source.node.clone(),
            process: wit.source.process.clone(),
        },
        timestamp: wit.timestamp.clone(),
        content: de_wit_error_content(&wit.content),
    }
}

fn en_wit_error(dewit: &UqbarError, context: String) -> WitUqbarError {
    WitUqbarError {
        source: WitProcessNode {
            node: dewit.source.node.clone(),
            process: dewit.source.process.clone(),
        },
        timestamp: dewit.timestamp.clone(),
        content: en_wit_error_content(&dewit.content, context),
    }
}

async fn get_and_send_specific_loop_message_to_process(
    awaited_message_id: u64,
    message_queue: &mut VecDeque<WrappedMessage>,
    recv_in_process: &mut MessageReceiver,
    send_to_terminal: &mut PrintSender,
    contexts: &HashMap<u64, ProcessContext>,
) -> (WrappedMessage, Result<Result<WitMessage, WitUqbarError>>) {
    loop {
        let wrapped_message = recv_in_process.recv().await.unwrap();
        //  if message id matches the one we sent out
        //   AND the message is not a websocket ack
        if awaited_message_id == wrapped_message.id {
            match wrapped_message.message {
                Ok(ref message) => {
                    if !(("net" == message.source.process)
                         & (Some(serde_json::Value::String("Success".into())) == message.content.payload.json)
                        )
                    {
                        let message = send_loop_message_to_process(
                            wrapped_message,
                            send_to_terminal,
                            contexts,
                        ).await;
                        return (
                            message.0,
                            match message.1 {
                                Ok(r) => {
                                    Ok(Ok(r.0))
                                },
                                Err(e) => Ok(Err(e)),
                            },
                        );
                    }
                },
                Err(_) => {
                    let message = send_loop_message_to_process(
                        wrapped_message,
                        send_to_terminal,
                        contexts,
                    ).await;
                    return (
                        message.0,
                        match message.1 {
                            Ok(r) => {
                                Ok(Ok(r.0))
                            },
                            Err(e) => Ok(Err(e)),
                        },
                    );
                }
            }
        }

        message_queue.push_back(wrapped_message);
    }
}

async fn get_and_send_loop_message_to_process(
    message_queue: &mut VecDeque<WrappedMessage>,
    recv_in_process: &mut MessageReceiver,
    send_to_terminal: &mut PrintSender,
    contexts: &HashMap<u64, ProcessContext>,
) -> (WrappedMessage, Result<Result<(WitMessage, String), WitUqbarError>>) {
    //  TODO: dont unwrap: causes panic when Start already running process
    let wrapped_message = recv_in_process.recv().await.unwrap();
    let wrapped_message =
        match message_queue.pop_front() {
            Some(m) => {
                message_queue.push_back(wrapped_message);
                m
            },
            None => wrapped_message,
        };

    let to_loop = send_loop_message_to_process(
        wrapped_message,
        send_to_terminal,
        contexts,
    ).await;

    (to_loop.0, Ok(to_loop.1))
}

async fn send_loop_message_to_process(
    wrapped_message: WrappedMessage,
    send_to_terminal: &mut PrintSender,
    contexts: &HashMap<u64, ProcessContext>,
) -> (WrappedMessage, Result<(WitMessage, String), WitUqbarError>) {
    match wrapped_message.message {
        Ok(ref message) => {
            let wit_message = convert_message_to_wit_message(message).await;

            let message_id = wrapped_message.id.clone();
            let message_type = message.content.message_type.clone();
            (
                wrapped_message,
                match message_type {
                    MessageType::Request(_) => Ok((wit_message, "".to_string())),
                    MessageType::Response => {
                        let context = get_context(&message_id, send_to_terminal, contexts)
                            .await;
                        Ok((wit_message, context))
                    },
                },
            )
        },
        Err(ref e) => {
            let context = get_context(&wrapped_message.id, send_to_terminal, contexts).await;
            let e = e.clone();
            (
                wrapped_message,
                Err(en_wit_error(&e, context)),
            )
        },
    }
}

async fn get_context(
    message_id: &u64,
    send_to_terminal: &mut PrintSender,
    contexts: &HashMap<u64, ProcessContext>,
) -> String {
    match contexts.get(&message_id) {
        Some(ref context) => {
            serde_json::to_string(&context.context).unwrap()
        },
        None => {
            send_to_terminal
                .send(Printout { verbosity: 1, content: "couldn't find context for Response".into() })
                .await
                .unwrap();
            "".into()
        },
    }
}

fn handle_request_case(
    is_expecting_response: bool,
    prompting_message: &Option<WrappedMessage>,
) -> (u64, Rsvp) {
    if is_expecting_response {
        (rand::random(), None)
    } else {
        //  rsvp is set if there was a Request expecting Response
        //   followed by Request(s) not expecting Response;
        //   could also be None if entire chain of Requests are
        //   not expecting Response
        match prompting_message {
            Some(ref prompting_message) => {
                let Ok(ref pm_message) = prompting_message.message else {
                    return (rand::random(), None);
                };
                match pm_message.content.message_type {
                    MessageType::Request(prompting_message_is_expecting_response) => {
                        if prompting_message_is_expecting_response {
                            (
                                prompting_message.id.clone(), //  TODO: need to reference count? ; is this right? I think we may want new message id here
                                Some(pm_message.source.clone()),
                            )
                        } else {
                            (
                                prompting_message.id.clone(),  //  TODO: need to reference count? ; is this right? I think we may want new message id here
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
            None => (rand::random(), None),
        }
    }
}

fn handle_response_case(
    prompting_message: &WrappedMessage,
    contexts: &HashMap<u64, ProcessContext>,
) -> Option<(u64, ProcessNode)> {
    match prompting_message.message {
        Ok(ref pm_message) => {
            match pm_message.content.message_type {
                MessageType::Request(is_expecting_response) => {
                    if is_expecting_response {
                        Some((
                            prompting_message.id.clone(),
                            pm_message.source.clone(),
                        ))
                    } else {
                        let Some(rsvp) = prompting_message.rsvp.clone() else {
                            println!("no rsvp set for response (prompting)");
                            return None;
                        };

                        Some((
                            prompting_message.id.clone(),
                            rsvp.clone(),
                        ))
                    }
                },
                MessageType::Response => {
                    let Some(context) = contexts.get(&prompting_message.id) else {
                        println!("couldn't find context to route response");
                        return None;
                    };
                    // println!("sprtl: resp to resp; prox, ult: {:?}, {:?}", context.proximate, context.ultimate);
                    let Some(ref ultimate) = context.ultimate else {
                        println!("couldn't find ultimate cause to route response");
                        return None;
                    };

                    match ultimate.message_type {
                        MessageType::Request(is_expecting_response) => {
                            if is_expecting_response {
                                Some((
                                    ultimate.id.clone(),
                                    ultimate.process_node.clone(),
                                ))
                            } else {
                                let Some(rsvp) = ultimate.rsvp.clone() else {
                                    println!("no rsvp set for response (ultimate)");
                                    return None;
                                };
                                Some((
                                    ultimate.id.clone(),
                                    rsvp.clone(),
                                ))
                            }
                        },
                        MessageType::Response => {
                            println!("ultimate as response unexpected case");
                            return None;
                        },
                    }
                },
            }
        },
        Err(ref e) => {
            let Some(context) = contexts.get(&prompting_message.id) else {
                println!("couldn't find context to route response");
                return None;
            };
            println!("sprtl: resp to resp; prox, ult: {:?}, {:?}", context.proximate, context.ultimate);
            let Some(ref ultimate) = context.ultimate else {
                println!("couldn't find ultimate cause to route response");
                return None;
            };

            match ultimate.message_type {
                MessageType::Request(is_expecting_response) => {
                    if is_expecting_response {
                        Some((
                            ultimate.id.clone(),
                            ultimate.process_node.clone(),
                        ))
                    } else {
                        let Some(rsvp) = ultimate.rsvp.clone() else {
                            println!("no rsvp set for response (ultimate)");
                            return None;
                        };
                        Some((
                            ultimate.id.clone(),
                            rsvp.clone(),
                        ))
                    }
                },
                MessageType::Response => {
                    println!("ultimate as response unexpected case");
                    return None;
                },
            }
        },
    }
}

fn make_wrapped_message(
    id: u64,
    source: ProcessNode,
    target: ProcessNode,
    rsvp: Rsvp,
    message_type: MessageType,
    payload: &WitPayload,
) -> WrappedMessage {
    let payload = Payload {
        json: match payload.json {
            Some(ref json_string) => serde_json::from_str(&json_string).unwrap_or(None),
            None => None,
        },
        bytes: payload.bytes.clone(),
    };
    WrappedMessage {
        id,
        target,
        rsvp,
        message: Ok(Message {
            source,
            content: MessageContent {
                message_type,
                payload,
            },
        })
    }
}

async fn send_process_results_to_loop(
    results: Result<Vec<(WitProtomessage, String)>, WitUqbarErrorContent>,
    source: ProcessNode,
    send_to_loop: MessageSender,
    prompting_message: &Option<WrappedMessage>,
    contexts: &mut HashMap<u64, ProcessContext>,
) -> Vec<u64> {
    let Ok(results) = results else {
        //  error case
        //  TODO: factor out the Response routing, since we want
        //        to route Error in the same way
        let Err(e) = results else { panic!("") };

        let Some(ref prompting_message) = prompting_message else {
            println!("need non-None prompting_message to handle Error");
            return vec![];  //  TODO: how to handle error on error routing case?
        };

        let (id, target) = match handle_response_case(
            &prompting_message,
            contexts,
        ) {
            Some(r) => r,
            None => return vec![],  //  TODO: how to handle error on error routing case?
        };

        let wrapped_message = WrappedMessage {
            id,
            target,
            rsvp: None,
            message: Err(UqbarError{
                source,
                timestamp: get_current_unix_time().unwrap(),
                content: de_wit_error_content(&e),
            }),
        };

        send_to_loop
            .send(wrapped_message)
            .await
            .unwrap();

        return vec![id];
    };

    let mut ids: Vec<u64> = Vec::new();
    for (WitProtomessage { protomessage_type, payload }, new_context_string) in &results {
        let new_context = serde_json::from_str(new_context_string).ok();
        let (id, rsvp, target, message_type) =
            match protomessage_type {
                WitProtomessageType::Request(type_with_target) => {
                    let (id, rsvp) = handle_request_case(
                        type_with_target.is_expecting_response,
                        &prompting_message,
                    );
                    (
                        id,
                        rsvp,
                        de_wit_process_node(&type_with_target.target),
                        // type_with_target.target_ship.clone(),
                        // type_with_target.target_app.clone(),
                        MessageType::Request(type_with_target.is_expecting_response),
                    )
                },
                WitProtomessageType::Response => {
                    let Some(ref prompting_message) = prompting_message else {
                        println!("need non-None prompting_message to handle Response");
                        continue;
                    };
                    let (id, target) = match handle_response_case(
                        &prompting_message,
                        contexts,
                    ) {
                        Some(r) => r,
                        None => continue,
                    };
                    (
                        id,
                        None,
                        target,
                        MessageType::Response,
                    )
                },
            };

        let wrapped_message = make_wrapped_message(
            id.clone(),
            source.clone(),
            target,
            rsvp,
            message_type,
            payload,
        );

        //  modify contexts if necessary
        //   note that this could be rolled into the `match` making Message above;
        //   if performance is bad here, roll into above
        let Ok(ref message) = wrapped_message.message else {
            panic!("foo");
        };
        match message.content.message_type {
            MessageType::Request(_) => {
                //  add context, as appropriate
                match prompting_message {
                    Some(ref prompting_message) => {
                        match prompting_message.message {
                            Ok(ref prompting_message_message) => {
                                match prompting_message_message.content.message_type {
                                    MessageType::Request(_) => {
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
                                        //  TODO: factor out with Eer case below
                                        match contexts.get(&prompting_message.id) {
                                            Some(context) => {
                                                //  ultimate is the ultimate of the prompt of Response
                                                contexts.insert(
                                                    wrapped_message.id,
                                                    ProcessContext {
                                                        proximate: CauseMetadata::new(
                                                            &wrapped_message
                                                        ),
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
                            Err(_) => {
                                //  TODO: factor out with Response case above
                                match contexts.get(&prompting_message.id) {
                                    Some(context) => {
                                        //  ultimate is the ultimate of the prompt of Response
                                        contexts.insert(
                                            wrapped_message.id,
                                            ProcessContext {
                                                proximate: CauseMetadata::new(
                                                    &wrapped_message
                                                ),
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
        json: match m.content.payload.json.clone() {
            Some(value) => Some(json_to_string(&value)),
            None => None,
        },
        bytes: m.content.payload.bytes.clone(),
    };
    let wit_message_type = match m.content.message_type {
        MessageType::Request(is_expecting_response) => {
            WitMessageType::Request(is_expecting_response)
        },
        MessageType::Response => WitMessageType::Response,
    };
    WitMessage {
        source: en_wit_process_node(&m.source),
        content: WitMessageContent {
            message_type: wit_message_type,
            payload: wit_payload,

        },
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
    let our_name = metadata.our.node.clone();
    let process_name = metadata.our.process.clone();

    let component = Component::new(&engine, &wasm_bytes)
        .expect("make_process_loop: couldn't read file");

    let mut linker = Linker::new(&engine);
    MicrokernelProcess::add_to_linker(&mut linker, |state: &mut ProcessWasi| state).unwrap();

    let mut table = Table::new();
    let wasi = WasiCtxBuilder::new().inherit_stdio().build(&mut table).unwrap();

    wasi::command::add_to_linker(&mut linker).unwrap();
    let mut store = Store::new(
        engine,
        ProcessWasi {
            process: Process {
                metadata,
                recv_in_process,
                send_to_loop: send_to_loop.clone(),
                send_to_terminal: send_to_terminal.clone(),
                prompting_message: None,
                contexts: HashMap::new(),
                message_queue: VecDeque::new(),
            },
            table,
            wasi,
        },
    );

    Box::pin(
        async move {
            let (bindings, _bindings) = MicrokernelProcess::instantiate_async(
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
                    target: ProcessNode {
                        node: our_name.clone(),
                        process: "process_manager".into(),
                    },
                    rsvp: None,
                    message: Ok(Message {
                        source: ProcessNode {
                            node: our_name.clone(),
                            process: "kernel".into(),
                        },
                        content: MessageContent {
                            message_type: MessageType::Request(false),
                            payload: Payload {
                                json: Some(serde_json::json!({
                                    "type": "Stop",
                                    "process_name": process_name,
                                })),
                                bytes: None,
                            },
                        },
                    }),
                })
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
    let Ok(message) = wrapped_message.message else {
        panic!("fubar");  //  TODO
    };
    let Some(value) = message.content.payload.json else {
        send_to_terminal
            .send(Printout{
                verbosity: 0,
                content: format!(
                    "kernel: got kernel command with no json payload. Got: {}",
                    message,
                ),
            })
            .await
            .unwrap();
        return;
    };
    let kernel_request: KernelRequest =
        serde_json::from_value(value)
        .expect("kernel: could not parse to command");
    match kernel_request {
        KernelRequest::StartProcess(cmd) => {
            let Some(wasm_bytes) = message.content.payload.bytes else {
                send_to_terminal
                    .send(Printout{
                        verbosity: 0,
                        content: format!(
                            "kernel: StartProcess requires bytes",
                        ),
                    })
                    .await
                    .unwrap();
                return;
            };
            let (send_to_process, recv_in_process) =
                mpsc::channel::<WrappedMessage>(PROCESS_CHANNEL_CAPACITY);
            senders.insert(cmd.process_name.clone(), send_to_process);
            let metadata = ProcessMetadata {
                our: ProcessNode {
                    node: our_name.to_string(),
                    process: cmd.process_name.clone(),
                },
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
                target: ProcessNode {
                    node: our_name.clone(),
                    process: "process_manager".into(),
                },
                rsvp: None,
                message: Ok(Message {
                    source: ProcessNode {
                        node: our_name.clone(),
                        process: "kernel".into(),
                    },
                    content: MessageContent {
                        message_type: MessageType::Response,
                        payload: Payload {
                            json: Some(
                                serde_json::to_value(
                                    KernelResponse::StartProcess(metadata)
                                ).unwrap()
                            ),
                            bytes: None,
                        },
                    },
                })
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
            let MessageType::Request(is_expecting_response) = message.content.message_type else {
                send_to_terminal
                    .send(Printout {
                        verbosity: 0,
                        content: "kernel: StopProcess requires Request, got Response".into()
                    })
                    .await
                    .unwrap();
                return;
            };
            let _ = senders.remove(&cmd.process_name);
            let process_handle = match process_handles.remove(&cmd.process_name) {
                Some(ph) => ph,
                None => {
                    send_to_terminal
                        .send(Printout {
                            verbosity: 0,
                            content: format!(
                                "kernel: no such process {} to Stop",
                                cmd.process_name,
                            ),
                        })
                        .await
                        .unwrap();
                    return;
                },
            };
            process_handle.abort();
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
                target: ProcessNode {
                    node: our_name.clone(),
                    process: "process_manager".into(),
                },
                rsvp: None,
                message: Ok(Message {
                    source: ProcessNode {
                        node: our_name.clone(),
                        process: "kernel".into(),
                    },
                    content: MessageContent {
                        message_type: MessageType::Response,
                        payload: Payload {
                            json: Some(json_payload),
                            bytes: None,
                        },
                    },
                })
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
    send_to_net: MessageSender,
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
                        if our_name != wrapped_message.target.node {
                            match send_to_net.send(wrapped_message).await {
                                Ok(()) => {
                                    send_to_terminal
                                        .send(Printout {
                                            verbosity: 1,
                                            content: "event loop: message sent to network".to_string(),
                                        })
                                        .await
                                        .unwrap();
                                }
                                Err(e) => {
                                    send_to_terminal
                                        .send(Printout {
                                            verbosity: 0,
                                            content: format!("event loop: message to network failed: {}", e),
                                        })
                                        .await
                                        .unwrap();
                                }
                            }
                        } else {
                            let to = wrapped_message.target.process.clone();
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
                            } else if to == "net" {
                                let _ = send_to_net.send(wrapped_message).await;
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
    our: Identity,
    // process_manager_wasm_path: String,
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
    config.cache_config_load_default().unwrap();
    config.wasm_backtrace_details(WasmBacktraceDetails::Enable);
    config.wasm_component_model(true);
    config.async_support(true);
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

    let our_kernel = ProcessNode {
        node: our.name.clone(),
        process: "kernel".into(),
    };

    // always start process_manager, terminal, http_bindings, apps_home on boot
    let processes = vec![
        "process_manager",
        "terminal",
        "http_bindings",
        "apps_home",
        "http_proxy",
    ];
    for process in &processes {
        let mut process_wasm_path = process.to_string();
        process_wasm_path.push_str(".wasm");
        let process_wasm_bytes = fs::read(&process_wasm_path).await.unwrap();
        let start_process_manager_message = WrappedMessage {
            id: rand::random(),
            target: our_kernel.clone(),
            rsvp: None,
            message: Ok(Message {
                source: our_kernel.clone(),
                content: MessageContent {
                    message_type: MessageType::Request(false),
                    payload: Payload {
                        json: Some(serde_json::to_value(
                            KernelRequest::StartProcess(
                                ProcessStart{
                                    process_name: (*process).into(),
                                    wasm_bytes_uri: process_wasm_path,
                                }
                            )
                        ).unwrap()),
                        bytes: Some(process_wasm_bytes),
                    },
                },
            })
        };
        send_to_loop.send(start_process_manager_message).await.unwrap();
    }

    // DEMO ONLY: start file_transfer app at boot
    if let Ok(ft_bytes) = fs::read("file_transfer.wasm").await {
        let start_apps_ft = WrappedMessage {
            id: rand::random(),
            target: our_kernel.clone(),
            rsvp: None,
            message: Ok(Message {
                source: our_kernel.clone(),
                content: MessageContent {
                    message_type: MessageType::Request(false),
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
            })
        };
        send_to_loop.send(start_apps_ft).await.unwrap();
    }

    let _ = event_loop_handle.await;
}
