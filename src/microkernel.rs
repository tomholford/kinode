use anyhow::Result;
use wasmtime::component::*;
use wasmtime::{Config, Engine, Store, WasmBacktraceDetails};
use tokio::sync::mpsc;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use tokio::task::JoinHandle;

use wasmtime_wasi::preview2::{Table, WasiCtx, WasiCtxBuilder, WasiView, wasi};

use crate::types::*;
//  WIT errors when `use`ing interface unless we import this and implement Host for Process below
use crate::microkernel::component::microkernel_process::types::Host;
use crate::microkernel::component::microkernel_process::types as wit;

bindgen!({
    path: "wit",
    world: "microkernel-process",
    async: true,
});
const PROCESS_CHANNEL_CAPACITY: usize = 100;

struct Process {
    metadata: ProcessMetadata,
    recv_in_process: MessageReceiver,
    send_to_loop: MessageSender,
    send_to_terminal: PrintSender,
    has_already_sent: HasAlreadySent,
    prompting_message: Option<WrappedMessage>,
    contexts: HashMap<u64, ProcessContext>,
    contexts_to_clean: Vec<u64>,  //  remove these upon receiving next message
    message_queue: VecDeque<WrappedMessage>,
}

struct ProcessWasi {
    process: Process,
    table: Table,
    wasi: WasiCtx,
}

enum HasAlreadySent {
    None,
    Requests,
    Response,
}

#[derive(Clone, Debug)]
struct CauseMetadata {
    id: u64,
    process_node: ProcessNode,
    rsvp: Rsvp,
    message_type: MessageType,
}

#[derive(Clone, Debug)]
struct ProcessContext {
    number_outstanding_requests: u32,
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
            number_outstanding_requests: 1,
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
    fn new_from_context(
        proximate: &WrappedMessage,
        ultimate: &Option<CauseMetadata>,
        context: Option<serde_json::Value>,
    ) -> Self {
        ProcessContext {
            number_outstanding_requests: 1,
            proximate: CauseMetadata::new(proximate),
            ultimate: ultimate.clone(),
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
    async fn send_requests(
        &mut self,
        requests: Result<(Vec<wit::WitProtorequest>, String), wit::WitUqbarErrorContent>,
    ) -> Result<()> {
        match self.process.has_already_sent {
            HasAlreadySent::Response => {
                return Err(anyhow::anyhow!("Cannot send Requests after Response"));
            },
            _ => {},
        }
        if let Ok(ref rs) = requests {
            for request in &rs.0 {
                if !is_valid_payload_bytes_from_process(&request.payload.bytes) {
                    return Err(
                        anyhow::anyhow!("Must have Some bytes when setting is_passthrough")
                    );
                }
            }
        };

        //  TODO: check if Circumvent::Receive & add bytes

        let _ = send_process_requests_to_loop(
            requests,
            self.process.metadata.our.clone(),
            &self.process.send_to_loop.clone(),
            &self.process.send_to_terminal.clone(),
            &self.process.prompting_message,
            &mut self.process.contexts,
        ).await?;

        self.process.has_already_sent = HasAlreadySent::Requests;
        Ok(())
    }

    async fn send_response(
        &mut self,
        response: Result<(wit::WitPayload, String), wit::WitUqbarErrorContent>,
    ) -> Result<()> {
        match self.process.has_already_sent {
            HasAlreadySent::None => {},
            _ => {
                return Err(anyhow::anyhow!("Cannot send Response after Requests or Response"));
            },
        }
        if let Ok(ref r) = response {
            if !is_valid_payload_bytes_from_process(&r.0.bytes) {
                return Err(anyhow::anyhow!("Must have Some bytes when setting is_passthrough"));
            }
        };

        let _ = send_process_response_to_loop(
            response,
            self.process.metadata.our.clone(),
            &self.process.send_to_loop,
            &self.process.send_to_terminal,
            &self.process.prompting_message,
            &mut self.process.contexts,
        ).await?;

        self.process.has_already_sent = HasAlreadySent::Response;
        Ok(())
    }

    async fn send_response_with_side_effect_request(
        &mut self,
        results: Result<wit::WitProtoresponseWithSideEffectProtorequest, wit::WitUqbarErrorContent>,
    ) -> Result<()> {
        match self.process.has_already_sent {
            HasAlreadySent::None => {},
            _ => {
                return Err(anyhow::anyhow!("Cannot send Response after Requests or Response"));
            },
        }
        if let Ok(ref rs) = results {
            if !is_valid_payload_bytes_from_process(&rs.response.0.bytes)
               | !is_valid_payload_bytes_from_process(&rs.request.0.payload.bytes) {
                return Err(anyhow::anyhow!("Must have Some bytes when setting is_passthrough"));
            }
        };

        let _ = send_process_response_with_side_effect_request_to_loop(
            results,
            self.process.metadata.our.clone(),
            &self.process.send_to_loop,
            &self.process.send_to_terminal,
            &self.process.prompting_message,
            &mut self.process.contexts,
        ).await?;

        self.process.has_already_sent = HasAlreadySent::Response;
        Ok(())
    }

    async fn await_next_message(&mut self)
    -> Result<Result<(wit::WitMessage, String), wit::WitUqbarError>> {
        let (wrapped_message, process_input) = get_and_send_loop_message_to_process(
            &mut self.process.message_queue,
            &mut self.process.recv_in_process,
            &mut self.process.send_to_terminal,
            &mut self.process.contexts,
            &mut self.process.contexts_to_clean,
        ).await;
        self.process.prompting_message = Some(wrapped_message);

        self.process.has_already_sent = HasAlreadySent::None;
        process_input
    }

    async fn send_request_and_await_response(
        &mut self,
        target: wit::WitProcessNode,
        payload: wit::WitPayload,
    ) -> Result<Result<wit::WitMessage, wit::WitUqbarError>> {
        let protomessage = wit::WitProtorequest {
            is_expecting_response: true,
            target,
            payload,
        };

        let ids = send_process_requests_to_loop(
            Ok((vec![protomessage], "".into())),
            self.process.metadata.our.clone(),
            &self.process.send_to_loop.clone(),
            &self.process.send_to_terminal.clone(),
            &self.process.prompting_message,
            &mut self.process.contexts,
        ).await?;

        if 1 != ids.len() {
            panic!("yield_and_await_response: must receive only 1 id back");
        }

        let (_wrapped_message, process_input) = get_and_send_specific_loop_message_to_process(
            ids[0],
            &mut self.process.message_queue,
            &mut self.process.recv_in_process,
            &mut self.process.send_to_terminal,
            &mut self.process.contexts,
            &mut self.process.contexts_to_clean,
        ).await;

        self.process.has_already_sent = HasAlreadySent::None;
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

fn de_wit_process_node(wit: &wit::WitProcessNode) -> ProcessNode {
    ProcessNode {
        node: wit.node.clone(),
        process: wit.process.clone(),
    }
}

fn en_wit_process_node(dewit: &ProcessNode) -> wit::WitProcessNode {
    wit::WitProcessNode {
        node: dewit.node.clone(),
        process: dewit.process.clone(),
    }
}

fn de_wit_error_content(wit: &wit::WitUqbarErrorContent) -> UqbarErrorContent {
    UqbarErrorContent {
        kind: wit.kind.clone(),
        message: serde_json::from_str(&wit.message).unwrap(),  //  TODO: handle error?
        context: serde_json::from_str(&wit.context).unwrap(),  //  TODO: handle error?
    }
}

fn en_wit_error_content(dewit: &UqbarErrorContent, context: String)
-> wit::WitUqbarErrorContent {
    wit::WitUqbarErrorContent {
        kind: dewit.kind.clone(),
        message: dewit.message.to_string(),
        context: if "" == context {
            dewit.context.to_string()
        } else {
            context
        }
    }
}

fn de_wit_error(wit: &wit::WitUqbarError) -> UqbarError {
    UqbarError {
        source: ProcessNode {
            node: wit.source.node.clone(),
            process: wit.source.process.clone(),
        },
        timestamp: wit.timestamp.clone(),
        content: de_wit_error_content(&wit.content),
    }
}

fn en_wit_error(dewit: &UqbarError, context: String) -> wit::WitUqbarError {
    wit::WitUqbarError {
        source: wit::WitProcessNode {
            node: dewit.source.node.clone(),
            process: dewit.source.process.clone(),
        },
        timestamp: dewit.timestamp.clone(),
        content: en_wit_error_content(&dewit.content, context),
    }
}

fn is_valid_payload_bytes_from_process(bytes: &wit::WitPayloadBytes) -> bool {
    match bytes.circumvent {
        wit::WitCircumvent::False => {
            //  do not circumvent: acceptable
            true
        },
        wit::WitCircumvent::Send => {
            //  must have Some content
            match bytes.content {
                Some(_) => true,
                None => false,
            }
        },
        wit::WitCircumvent::Circumvented => {
            //  kernel must set: user is not allowed to set
            false
        },
        wit::WitCircumvent::Receive => {
            //  apply already-circumvented bytes to message: acceptable
            true
        },
    }
}

async fn clean_contexts(
    send_to_terminal: &PrintSender,
    contexts: &mut HashMap<u64, ProcessContext>,
    contexts_to_clean: &mut Vec<u64>,
) {
    for id in contexts_to_clean.drain(..) {
        send_to_terminal.send(Printout {
            verbosity: 1,
            content: format!(
                "cleaned up context {}",
                id,
            ),
        }).await.unwrap();
        let _ = contexts.remove(&id);
    }
    send_to_terminal.send(Printout {
        verbosity: 1,
        content: format!(
            "contexts now reads: {:?}",
            contexts,
        ),
    }).await.unwrap();
}

async fn get_and_send_specific_loop_message_to_process(
    awaited_message_id: u64,
    message_queue: &mut VecDeque<WrappedMessage>,
    recv_in_process: &mut MessageReceiver,
    send_to_terminal: &mut PrintSender,
    contexts: &mut HashMap<u64, ProcessContext>,
    contexts_to_clean: &mut Vec<u64>,
) -> (WrappedMessage, Result<Result<wit::WitMessage, wit::WitUqbarError>>) {
    clean_contexts(send_to_terminal, contexts, contexts_to_clean).await;
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
                            contexts_to_clean,
                        ).await;
                        return (
                            message.0,
                            match message.1 {
                                Ok(r) => Ok(Ok(r.0)),
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
                        contexts_to_clean,
                    ).await;
                    return (
                        message.0,
                        match message.1 {
                            Ok(r) => Ok(Ok(r.0)),
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
    contexts: &mut HashMap<u64, ProcessContext>,
    contexts_to_clean: &mut Vec<u64>,
) -> (WrappedMessage, Result<Result<(wit::WitMessage, String), wit::WitUqbarError>>) {
    clean_contexts(send_to_terminal, contexts, contexts_to_clean).await;
    //  TODO: dont unwrap: causes panic when Start already running process
    let wrapped_message = recv_in_process.recv().await.unwrap();
    let wrapped_message = match message_queue.pop_front() {
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
        contexts_to_clean,
    ).await;

    (to_loop.0, Ok(to_loop.1))
}

async fn send_loop_message_to_process(
    wrapped_message: WrappedMessage,
    send_to_terminal: &mut PrintSender,
    contexts: &mut HashMap<u64, ProcessContext>,
    contexts_to_clean: &mut Vec<u64>,
) -> (WrappedMessage, Result<(wit::WitMessage, String), wit::WitUqbarError>) {
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
                        let context = get_context(
                            &message_id,
                            send_to_terminal,
                            contexts,
                            contexts_to_clean,
                        ).await;
                        Ok((wit_message, context))
                    },
                },
            )
        },
        Err(ref e) => {
            let context = get_context(
                &wrapped_message.id,
                send_to_terminal,
                contexts,
                contexts_to_clean,
            ).await;
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
    contexts: &mut HashMap<u64, ProcessContext>,
    contexts_to_clean: &mut Vec<u64>,
) -> String {
    decrement_context(message_id, send_to_terminal, contexts, contexts_to_clean).await;
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

fn make_request_id_target(
    default_id: u64,
    is_expecting_response: bool,
    prompting_message: &Option<WrappedMessage>,
) -> (u64, Rsvp) {
    if is_expecting_response {
        (default_id, None)
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
                                prompting_message.id.clone(),
                                Some(pm_message.source.clone()),
                            )
                        } else {
                            (
                                prompting_message.id.clone(),
                                prompting_message.rsvp.clone(),
                            )
                        }
                    },
                    MessageType::Response => (rand::random(), None),
                }
            },
            None => (rand::random(), None),
        }
    }
}

async fn make_response_id_target(
    prompting_message: &Option<WrappedMessage>,
    contexts: &HashMap<u64, ProcessContext>,
    send_to_terminal: PrintSender,
) -> Option<(u64, ProcessNode)> {
    let Some(ref prompting_message) = prompting_message else {
        println!("need non-None prompting_message to handle Response");
        return None;
    };
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
                            send_to_terminal
                                .send(Printout {
                                    verbosity: 1,
                                    content: "no rsvp set for response (prompting)".into(),
                                })
                                .await
                                .unwrap();
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
                        send_to_terminal
                            .send(Printout {
                                verbosity: 0,
                                content: "couldn't find context to route response".into(),
                            })
                            .await
                            .unwrap();
                        return None;
                    };
                    let Some(ref ultimate) = context.ultimate else {
                        send_to_terminal
                            .send(Printout {
                                verbosity: 0,
                                content: "couldn't find ultimate cause to route response"
                                    .into(),
                            })
                            .await
                            .unwrap();
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
                                    send_to_terminal
                                        .send(Printout {
                                            verbosity: 1,
                                            content: "no rsvp set for response (ultimate)"
                                                .into(),
                                        })
                                        .await
                                        .unwrap();
                                    return None;
                                };
                                Some((
                                    ultimate.id.clone(),
                                    rsvp.clone(),
                                ))
                            }
                        },
                        MessageType::Response => {
                            send_to_terminal
                                .send(Printout {
                                    verbosity: 0,
                                    content: "ultimate as response unexpected case".into(),
                                })
                                .await
                                .unwrap();
                            return None;
                        },
                    }
                },
            }
        },
        Err(ref e) => {
            let Some(context) = contexts.get(&prompting_message.id) else {
                send_to_terminal
                    .send(Printout {
                        verbosity: 0,
                        content: "couldn't find context to route response (err)".into(),
                    })
                    .await
                    .unwrap();
                return None;
            };
            let Some(ref ultimate) = context.ultimate else {
                send_to_terminal
                    .send(Printout {
                        verbosity: 0,
                        content: "couldn't find ultimate cause to route response (err)"
                            .into(),
                    })
                    .await
                    .unwrap();
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
                        send_to_terminal
                            .send(Printout {
                                verbosity: 1,
                                content: "no rsvp set for response (ultimate) (err)".into(),
                            })
                            .await
                            .unwrap();
                            return None;
                        };
                        Some((
                            ultimate.id.clone(),
                            rsvp.clone(),
                        ))
                    }
                },
                MessageType::Response => {
                    send_to_terminal
                        .send(Printout {
                            verbosity: 0,
                            content: "ultimate as response unexpected case (err)".into(),
                        })
                        .await
                        .unwrap();
                    return None;
                },
            }
        },
    }
}

fn de_wit_payload_bytes(wit: &wit::WitPayloadBytes) -> PayloadBytes {
    PayloadBytes {
        circumvent: match wit.circumvent {
            wit::WitCircumvent::False => Circumvent::False,
            wit::WitCircumvent::Send => Circumvent::Send,
            wit::WitCircumvent::Circumvented => Circumvent::Circumvented,
            wit::WitCircumvent::Receive => Circumvent::Receive,
        },
        content: wit.content.clone(),
    }
}

fn make_wrapped_message(
    id: u64,
    source: ProcessNode,
    target: ProcessNode,
    rsvp: Rsvp,
    message_type: MessageType,
    payload: &wit::WitPayload,
) -> WrappedMessage {
    let payload = Payload {
        json: match payload.json {
            Some(ref json_string) => serde_json::from_str(&json_string).unwrap_or(None),
            None => None,
        },
        bytes: de_wit_payload_bytes(&payload.bytes),
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

async fn insert_or_increment_context(
    is_expecting_response: bool,
    id: u64,
    context: ProcessContext,
    send_to_terminal: &PrintSender,
    contexts: &mut HashMap<u64, ProcessContext>,
) {
    if is_expecting_response {
        match contexts.remove(&id) {
            Some(mut existing_context) => {
                existing_context.number_outstanding_requests += 1;
                send_to_terminal.send(Printout {
                    verbosity: 1,
                    content: format!(
                        "context for {} inc to {}; {:?}",
                        id,
                        existing_context.number_outstanding_requests,
                        existing_context,
                    ),
                }).await.unwrap();
                contexts.insert(id, existing_context);
            },
            None => {
                send_to_terminal.send(Printout {
                    verbosity: 1,
                    content: format!(
                        "context for {} inc to {}; {:?}",
                        id,
                        1,
                        context,
                    ),
                }).await.unwrap();
                contexts.insert(id, context);
            }
        }
    }
}

async fn decrement_context(
    id: &u64,
    send_to_terminal: &PrintSender,
    contexts: &mut HashMap<u64, ProcessContext>,
    contexts_to_clean: &mut Vec<u64>,
) {
    match contexts.get_mut(id) {
        Some(context) => {
            context.number_outstanding_requests -= 1;
            if 0 == context.number_outstanding_requests {
                //  remove context upon receiving next message
                contexts_to_clean.push(id.clone());
            }
            send_to_terminal.send(Printout {
                verbosity: 1,
                content: format!(
                    "context for {} dec to {}",
                    id,
                    context.number_outstanding_requests,
                ),
            }).await.unwrap();
        },
        None => {
            send_to_terminal.send(Printout {
                verbosity: 1,
                content: format!(
                    "decrement_context: {} not found",
                    id,
                ),
            }).await.unwrap();
        },
    }
}

async fn send_process_error_to_loop(
    error: wit::WitUqbarErrorContent,
    source: ProcessNode,
    send_to_loop: &MessageSender,
    prompting_message: &Option<WrappedMessage>,
) -> Vec<u64> {
    let Some(ref prompting_message) = prompting_message else {
        println!("need non-None prompting_message to handle Error");
        return vec![];  //  TODO: how to handle error on error routing case?
    };

    let id = prompting_message.id.clone();
    let target = match prompting_message.message {
        Ok(ref m) => m.source.clone(),
        Err(ref e) => e.source.clone(),
    };

    let wrapped_message = WrappedMessage {
        id,
        target,
        rsvp: None,
        message: Err(UqbarError {
            source,
            timestamp: get_current_unix_time().unwrap(),
            content: de_wit_error_content(&error),
        }),
    };

    send_to_loop
        .send(wrapped_message)
        .await
        .unwrap();

    return vec![id];
}

async fn handle_request(
    source: ProcessNode,
    send_to_loop: &MessageSender,
    send_to_terminal: &PrintSender,
    default_id: u64,
    is_expecting_response: bool,
    target: wit::WitProcessNode,
    payload: wit::WitPayload,
    prompting_message: &Option<WrappedMessage>,
    new_context: Option<serde_json::Value>,
    contexts: &mut HashMap<u64, ProcessContext>,
) -> Result<u64> {
    let (id, rsvp) = make_request_id_target(
        default_id,
        is_expecting_response,
        &prompting_message,
    );
    let target = de_wit_process_node(&target);

    let payload: wit::WitPayload = if_circumvent_update_bytes_with_circumvent(
        payload,
        prompting_message,
    )?;

    let wrapped_message = make_wrapped_message(
        id.clone(),
        source.clone(),
        target,
        rsvp,
        MessageType::Request(is_expecting_response),
        &payload,
    );

    //  modify contexts
    let process_context = match prompting_message {
        Some(ref prompting_message) => {
            match prompting_message.message {
                Ok(ref prompting_message_message) => {
                    match prompting_message_message.content.message_type {
                        MessageType::Request(_) => {
                            //  case: prompting_message_is_expecting_response
                            //   ultimate stored for source
                            //  case: !prompting_message_is_expecting_response
                            //   ultimate stored for rsvp
                            ProcessContext::new(
                                &wrapped_message,
                                Some(&prompting_message),
                                new_context,
                            )
                        },
                        MessageType::Response => {
                            match contexts.get(&prompting_message.id) {
                                Some(context) => {
                                    //  ultimate is the ultimate of the prompt of Response
                                    ProcessContext::new_from_context(
                                        &wrapped_message,
                                        &context.ultimate,
                                        new_context,
                                    )
                                },
                                None => {
                                    //  should this even be allowed?
                                    ProcessContext::new(
                                        &wrapped_message,
                                        Some(&prompting_message),
                                        new_context,
                                    )
                                },
                            }
                        },
                    }
                },
                Err(_) => {
                    match contexts.get(&prompting_message.id) {
                        Some(context) => {
                            //  ultimate is the ultimate of the prompt of Response
                            ProcessContext::new_from_context(
                                &wrapped_message,
                                &context.ultimate,
                                new_context,
                            )
                        },
                        None => {
                            //  should this even be allowed?
                            ProcessContext::new(
                                &wrapped_message,
                                Some(&prompting_message),
                                new_context,
                            )
                        },
                    }
                },
            }
        },
        None => {
            ProcessContext::new(
                &wrapped_message,
                None,
                new_context,
            )
        },
    };
    insert_or_increment_context(
        is_expecting_response,
        wrapped_message.id,
        process_context,
        &send_to_terminal,
        contexts,
    ).await;

    send_to_loop
        .send(wrapped_message)
        .await
        .unwrap();

    Ok(id)
}

fn if_circumvent_update_bytes_with_circumvent(
    mut payload: wit::WitPayload,
    prompting_message: &Option<WrappedMessage>,
) -> Result<wit::WitPayload> {
    match payload.bytes.circumvent {
        wit::WitCircumvent::Receive => {
            let Some(pm) = prompting_message else {
                return Err(anyhow::anyhow!(""));
            };
            let Ok(ref m) = pm.message else {
                return Err(anyhow::anyhow!(""));
            };
            let Some(ref b) = m.content.payload.bytes.content else {
                return Err(anyhow::anyhow!(""));
            };
            payload.bytes.content = Some(b.clone());
            return Ok(payload);
        },
        _ => Ok(payload),
    }
}

async fn send_process_requests_to_loop(
    requests: Result<(Vec<wit::WitProtorequest>, String), wit::WitUqbarErrorContent>,
    source: ProcessNode,
    send_to_loop: &MessageSender,
    send_to_terminal: &PrintSender,
    prompting_message: &Option<WrappedMessage>,
    contexts: &mut HashMap<u64, ProcessContext>,
) -> Result<Vec<u64>> {
    let Ok(requests) = requests else {
        let Err(e) = requests else { panic!("") };
        return Ok(
            send_process_error_to_loop(e, source, send_to_loop, prompting_message).await
        );
    };

    let mut ids: Vec<u64> = Vec::new();
    let default_id = rand::random();
    let new_context = serde_json::from_str(&requests.1).ok();
    for wit::WitProtorequest { is_expecting_response, target, payload } in requests.0 {
        ids.push(handle_request(
            source.clone(),
            send_to_loop,
            send_to_terminal,
            default_id,
            is_expecting_response,
            target,
            payload,
            prompting_message,
            new_context.clone(),
            contexts,
        ).await?);
    }
    Ok(ids)
}

async fn send_process_response_to_loop(
    response: Result<(wit::WitPayload, String), wit::WitUqbarErrorContent>,
    source: ProcessNode,
    send_to_loop: &MessageSender,
    send_to_terminal: &PrintSender,
    prompting_message: &Option<WrappedMessage>,
    contexts: &mut HashMap<u64, ProcessContext>,
) -> Result<u64> {
    let Ok(response) = response else {
        let Err(e) = response else { panic!("") };
        let ids = send_process_error_to_loop(e, source, send_to_loop, prompting_message)
            .await;
        return Ok(ids[0]);
    };

    let payload = response.0;
    // let new_context: Option<serde_json::Value> = serde_json::from_str(&response.1).ok();
    let (id, target) = match make_response_id_target(
        &prompting_message,
        contexts,
        send_to_terminal.clone(),
    ).await {
        Some(r) => r,
        None => {
            send_to_terminal
                .send(Printout {
                    verbosity: 1,
                    content: format!(
                        "dropping Response: {:?}",
                        payload.json,
                    ),
                })
                .await
                .unwrap();
            return Ok(0);  //  TODO: do better
        },
    };
    let rsvp = None;

    let payload: wit::WitPayload = if_circumvent_update_bytes_with_circumvent(
        payload,
        prompting_message,
    )?;

    let wrapped_message = make_wrapped_message(
        id.clone(),
        source.clone(),
        target,
        rsvp,
        MessageType::Response,
        &payload,
    );

    send_to_loop
        .send(wrapped_message)
        .await
        .unwrap();

    return Ok(id);
}

async fn send_process_response_with_side_effect_request_to_loop(
    results: Result<wit::WitProtoresponseWithSideEffectProtorequest, wit::WitUqbarErrorContent>,
    source: ProcessNode,
    send_to_loop: &MessageSender,
    send_to_terminal: &PrintSender,
    prompting_message: &Option<WrappedMessage>,
    contexts: &mut HashMap<u64, ProcessContext>,
) -> Result<Vec<u64>> {
    let Ok(results) = results else {
        let Err(e) = results else { panic!("") };
        return Ok(
            send_process_error_to_loop(e, source, send_to_loop, prompting_message).await
        );
    };

    //  handle Response
    let (payload, new_context) = results.response;

    let mut ids: Vec<u64> = Vec::new();

    let new_context = serde_json::from_str(&new_context).ok();
    let (id, target) = match make_response_id_target(
        &prompting_message,
        contexts,
        send_to_terminal.clone(),
    ).await {
        Some(r) => r,
        None => {
            send_to_terminal
                .send(Printout {
                    verbosity: 1,
                    content: format!(
                        "dropping Response: {:?}",
                        payload.json,
                    ),
                })
                .await
                .unwrap();
            return Ok(vec![]);
        },
    };
    let rsvp = None;

    let payload: wit::WitPayload = if_circumvent_update_bytes_with_circumvent(
        payload,
        prompting_message,
    )?;

    let wrapped_message = make_wrapped_message(
        id.clone(),
        source.clone(),
        target,
        rsvp,
        MessageType::Response,
        &payload,
    );

    send_to_loop
        .send(wrapped_message)
        .await
        .unwrap();

    ids.push(id);

    //  handle side-effect Request
    let (wit::WitProtorequest { is_expecting_response, target, payload }, request_context) =
        results.request;
    let default_id = rand::random();
    let prompting_message = None;

    ids.push(handle_request(
        source,
        send_to_loop,
        send_to_terminal,
        default_id,
        is_expecting_response,
        target,
        payload,
        &prompting_message,
        new_context,
        contexts,
    ).await?);

    return Ok(ids);
}

async fn convert_message_to_wit_message(m: &Message) -> wit::WitMessage {
    let wit_payload = wit::WitPayload {
        json: match m.content.payload.json.clone() {
            Some(value) => Some(json_to_string(&value)),
            None => None,
        },
        bytes: match m.content.payload.bytes.circumvent {
            Circumvent::False => {
                wit::WitPayloadBytes {
                    circumvent: wit::WitCircumvent::False,
                    content: m.content.payload.bytes.content.clone(),
                }
            },
            Circumvent::Send => {
                wit::WitPayloadBytes {
                    circumvent: wit::WitCircumvent::Circumvented,
                    content: None,
                }
            },
            Circumvent::Circumvented | Circumvent::Receive => {
                panic!("convert_message_to_wit_message")
            },
        },
    };
    let wit_message_type = match m.content.message_type {
        MessageType::Request(is_expecting_response) => {
            wit::WitMessageType::Request(is_expecting_response)
        },
        MessageType::Response => wit::WitMessageType::Response,
    };
    wit::WitMessage {
        source: en_wit_process_node(&m.source),
        content: wit::WitMessageContent {
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
    let wasm_bytes_uri = metadata.wasm_bytes_uri.clone();
    let send_on_panic = metadata.send_on_panic.clone();

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
                has_already_sent: HasAlreadySent::None,
                prompting_message: None,
                contexts: HashMap::new(),
                contexts_to_clean: Vec::new(),
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

            let our_pm = ProcessNode {
                node: our_name.clone(),
                process: "process_manager".into(),
            };
            let our_kernel = ProcessNode {
                node: our_name.clone(),
                process: "kernel".into(),  //  should this be process_name?
            };

            //  clean up process metadata & channels
            send_to_loop
                .send(WrappedMessage {
                    id: rand::random(),
                    target: our_pm.clone(),
                    rsvp: None,
                    message: Ok(Message {
                        source: our_kernel.clone(),
                        content: MessageContent {
                            message_type: MessageType::Request(false),
                            payload: Payload {
                                json: Some(
                                    serde_json::to_value(ProcessManagerCommand::Stop {
                                        process_name: process_name.clone()
                                    }).unwrap()
                                ),
                                bytes: PayloadBytes {
                                    circumvent: Circumvent::False,
                                    content: None
                                },
                            },
                        },
                    }),
                })
                .await
                .unwrap();

            match send_on_panic {
                SendOnPanic::None => {},
                SendOnPanic::Restart => {
                    send_to_loop
                        .send(WrappedMessage {
                            id: rand::random(),
                            target: our_pm,
                            rsvp: None,
                            message: Ok(Message {
                                source: our_kernel,
                                content: MessageContent {
                                    message_type: MessageType::Request(false),
                                    payload: Payload {
                                        json: Some(
                                            serde_json::to_value(ProcessManagerCommand::Start {
                                                process_name,
                                                wasm_bytes_uri,
                                                send_on_panic,
                                            }).unwrap()
                                        ),
                                        bytes: PayloadBytes {
                                            circumvent: Circumvent::False,
                                            content: None
                                        },
                                    },
                                },
                            }),
                        })
                        .await
                        .unwrap();
                },
                SendOnPanic::Requests(requests) => {
                    for request in requests {
                        send_to_loop
                            .send(WrappedMessage {
                                id: rand::random(),
                                target: request.target,
                                rsvp: None,
                                message: Ok(Message {
                                    source: ProcessNode {
                                        node: our_name.clone(),
                                        process: process_name.clone(),
                                    },
                                    content: MessageContent {
                                        message_type: MessageType::Request(false),
                                        payload: request.payload,
                                    },
                                }),
                            })
                            .await
                            .unwrap();
                    }
                },
            }
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
        KernelRequest::StartProcess { process_name, wasm_bytes_uri, send_on_panic } => {
            let Some(wasm_bytes) = message.content.payload.bytes.content else {
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
            senders.insert(process_name.clone(), send_to_process);
            let metadata = ProcessMetadata {
                our: ProcessNode {
                    node: our_name.to_string(),
                    process: process_name.clone(),
                },
                wasm_bytes_uri: wasm_bytes_uri.clone(),
                send_on_panic,
            };
            process_handles.insert(
                process_name.clone(),
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
                            bytes: PayloadBytes {
                                circumvent: Circumvent::False,
                                content: None
                            },
                        },
                    },
                })
            };
            send_to_loop
                .send(start_completed_message)
                .await
                .unwrap();
            return;
        },
        KernelRequest::StopProcess { process_name } => {
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
            let _ = senders.remove(&process_name);
            let process_handle = match process_handles.remove(&process_name) {
                Some(ph) => ph,
                None => {
                    send_to_terminal
                        .send(Printout {
                            verbosity: 0,
                            content: format!(
                                "kernel: no such process {} to Stop",
                                process_name,
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
                KernelResponse::StopProcess { process_name }
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
                            bytes: PayloadBytes {
                                circumvent: Circumvent::False,
                                content: None
                            },
                        },
                    },
                })
            };
            send_to_loop
                .send(stop_completed_message)
                .await
                .unwrap();
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
    send_to_lfs: MessageSender,
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
            senders.insert("lfs".to_string(), send_to_lfs);

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
    send_to_loop: MessageSender,
    send_to_terminal: PrintSender,
    recv_in_loop: MessageReceiver,
    recv_debug_in_loop: DebugReceiver,
    send_to_wss: MessageSender,
    send_to_fs: MessageSender,
    send_to_lfs: MessageSender,
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
            send_to_lfs,
            send_to_http_server,
            send_to_http_client,
            send_to_terminal.clone(),
            engine,
        ).await
    );

    let _ = event_loop_handle.await;
}
