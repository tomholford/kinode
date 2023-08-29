use anyhow::Result;
use wasmtime::component::*;
use wasmtime::{Config, Engine, Store, WasmBacktraceDetails};
use tokio::sync::mpsc;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use tokio::task::JoinHandle;

use wasmtime_wasi::preview2::{Table, WasiCtx, WasiCtxBuilder, WasiView, wasi};

use crate::types as t;
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
    metadata: t::ProcessMetadata,
    recv_in_process: t::MessageReceiver,
    send_to_loop: t::MessageSender,
    send_to_terminal: t::PrintSender,
    has_already_sent: bool,
    prompting_message: Option<t::KernelMessage>,
    contexts: HashMap<u64, ProcessContext>,
    contexts_to_clean: Vec<u64>,  //  remove these upon receiving next message
    message_queue: VecDeque<t::KernelMessage>,
}

struct ProcessWasi {
    process: Process,
    table: Table,
    wasi: WasiCtx,
}

#[derive(Clone, Debug)]
struct CauseMetadata {
    id: u64,
    source: t::ProcessReference,
    rsvp: t::Rsvp,
    is_expecting_response: bool,
    // message_type: t::MessageType,
}

#[derive(Clone, Debug)]
struct ProcessContext {
    number_outstanding_requests: u32,
    proximate: CauseMetadata,         //  kernel only: for routing responses  TODO: needed?
    ultimate: Option<CauseMetadata>,  //  kernel only: for routing responses
    context: serde_json::Value,       //  input/output from process
}

impl CauseMetadata {
    fn new(kernel_message: &t::KernelMessage) -> Self {
        let Ok(ref message) = kernel_message.message else { panic!("") };  //  TODO: need error?
        let t::TransitMessage::Request(ref r) = message else { panic!("cause not request") };
        CauseMetadata {
            id: kernel_message.id.clone(),
            source: r.payload.source.clone(),
            rsvp: kernel_message.rsvp.clone(),
            is_expecting_response: r.is_expecting_response,
        }
    }
}

impl ProcessContext {
    fn new(
        proximate: &t::KernelMessage,
        ultimate: Option<&t::KernelMessage>,
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
        proximate: &t::KernelMessage,
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
type Senders = HashMap<u64, t::MessageSender>;
type Names = HashMap<String, u64>;
type Ids = HashMap<u64, String>;
type ProcessHandles = HashMap<u64, JoinHandle<Result<()>>>;

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
    async fn receive(&mut self)
    -> Result<Result<(wit::InboundMessage, String), wit::UqbarError>> {
        let (kernel_message, process_input) = get_and_send_loop_message_to_process(
            &mut self.process.message_queue,
            &mut self.process.recv_in_process,
            &mut self.process.send_to_terminal,
            &mut self.process.contexts,
            &mut self.process.contexts_to_clean,
        ).await;
        self.process.prompting_message = Some(kernel_message);

        self.process.has_already_sent = false;
        process_input
    }

    async fn send(
        &mut self,
        message: Result<wit::OutboundMessage, wit::UqbarErrorPayload>,
    ) -> Result<()> {
        if self.process.has_already_sent {
            return Err(anyhow::anyhow!("Can only `send()` once per `receive()`."));
        }

        let Ok(message) = message else {
            let Err(e) = message else { panic!("") };
            let _ = send_process_error_to_loop(
                e,
                address_to_reference(&self.process.metadata.our),
                &self.process.send_to_loop,
                &self.process.prompting_message,
            ).await;
            return Ok(());
        };

        match message {
            wit::OutboundMessage::Requests(requests) => {
                let _ = send_process_requests_to_loop(
                    requests,
                    address_to_reference(&self.process.metadata.our),
                    &self.process.send_to_loop.clone(),
                    &self.process.send_to_terminal.clone(),
                    &self.process.prompting_message,
                    &mut self.process.contexts,
                ).await?;
            },
            wit::OutboundMessage::Response(response) => {
                let _ = send_process_response_to_loop(
                    response,
                    address_to_reference(&self.process.metadata.our),
                    &self.process.send_to_loop,
                    &self.process.send_to_terminal,
                    &self.process.prompting_message,
                    &mut self.process.contexts,
                ).await?;
            },
            wit::OutboundMessage::ResponseWithSideEffect(rwse) => {
                let _ = send_process_response_with_side_effect_request_to_loop(
                    rwse,
                    address_to_reference(&self.process.metadata.our),
                    &self.process.send_to_loop,
                    &self.process.send_to_terminal,
                    &self.process.prompting_message,
                    &mut self.process.contexts,
                ).await?;
            },
        }

        self.process.has_already_sent = true;
        Ok(())
    }

    async fn send_and_await_receive(
        &mut self,
        target: wit::ProcessReference,
        payload: wit::OutboundPayload,
    ) -> Result<Result<wit::InboundMessage, wit::UqbarError>> {
        let request = wit::OutboundRequest {
            is_expecting_response: true,
            target,
            payload,
        };

        let ids = send_process_requests_to_loop(
            vec![(vec![request], "".into())],
            address_to_reference(&self.process.metadata.our),
            &self.process.send_to_loop.clone(),
            &self.process.send_to_terminal.clone(),
            &self.process.prompting_message,
            &mut self.process.contexts,
        ).await?;

        if 1 != ids.len() {
            panic!("yield_and_await_response: must receive only 1 id back");
        }

        let (_kernel_message, process_input) = get_and_send_specific_loop_message_to_process(
            ids[0],
            &mut self.process.message_queue,
            &mut self.process.recv_in_process,
            &mut self.process.send_to_terminal,
            &mut self.process.contexts,
            &mut self.process.contexts_to_clean,
        ).await;

        self.process.has_already_sent = false;
        process_input
    }

    async fn print_to_terminal(&mut self, verbosity: u8, content: String) -> Result<()> {
        self.process.send_to_terminal
            .send(t::Printout { verbosity, content })
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

fn address_to_reference(address: &t::ProcessAddress) -> t::ProcessReference {
    t::ProcessReference {
        node: address.node.clone(),
        identifier: match address.name {
            None => t::ProcessIdentifier::Id(address.id.clone()),
            Some(ref n) => t::ProcessIdentifier::Name(n.clone()),
        },
    }
}

fn de_wit_process_identifier(wit: &wit::ProcessIdentifier) -> t::ProcessIdentifier {
    match wit {
        wit::ProcessIdentifier::Id(id) => t::ProcessIdentifier::Id(id.clone()),
        wit::ProcessIdentifier::Name(name) => t::ProcessIdentifier::Name(name.clone()),
    }
}

fn en_wit_process_identifier(dewit: &t::ProcessIdentifier) -> wit::ProcessIdentifier {
    match dewit {
        t::ProcessIdentifier::Id(id) => wit::ProcessIdentifier::Id(id.clone()),
        t::ProcessIdentifier::Name(name) => wit::ProcessIdentifier::Name(name.clone()),
    }
}

fn de_wit_process_reference(wit: &wit::ProcessReference) -> t::ProcessReference {
    t::ProcessReference {
        node: wit.node.clone(),
        identifier: de_wit_process_identifier(&wit.identifier),
    }
}

fn en_wit_process_reference(dewit: &t::ProcessReference) -> wit::ProcessReference {
    wit::ProcessReference {
        node: dewit.node.clone(),
        identifier: en_wit_process_identifier(&dewit.identifier),
    }
}

fn en_wit_process_address(dewit: &t::ProcessAddress) -> wit::ProcessAddress {
    wit::ProcessAddress {
        node: dewit.node.clone(),
        id: dewit.id.clone(),
        name: dewit.name.clone(),
    }
}

fn de_wit_error_payload(wit: &wit::UqbarErrorPayload) -> t::UqbarErrorPayload {
    t::UqbarErrorPayload {
        kind: wit.kind.clone(),
        message: serde_json::from_str(&wit.message).unwrap(),  //  TODO: handle error?
        context: serde_json::from_str(&wit.context).unwrap(),  //  TODO: handle error?
    }
}

fn en_wit_error_payload(dewit: &t::UqbarErrorPayload, context: String)
-> wit::UqbarErrorPayload {
    wit::UqbarErrorPayload {
        kind: dewit.kind.clone(),
        message: dewit.message.to_string(),
        context: if "" == context {
            dewit.context.to_string()
        } else {
            context
        }
    }
}

fn en_wit_error(dewit: &t::UqbarError, context: String) -> wit::UqbarError {
    wit::UqbarError {
        source: wit::ProcessReference {
            node: dewit.source.node.clone(),
            identifier: en_wit_process_identifier(&dewit.source.identifier),
        },
        timestamp: dewit.timestamp.clone(),
        payload: en_wit_error_payload(&dewit.payload, context),
    }
}

fn get_transit_message_source(message: &t::TransitMessage) -> &t::ProcessReference {
    match message {
        t::TransitMessage::Request(request) => &request.payload.source,
        t::TransitMessage::Response(payload) => &payload.source,
    }
}

async fn clean_contexts(
    send_to_terminal: &t::PrintSender,
    contexts: &mut HashMap<u64, ProcessContext>,
    contexts_to_clean: &mut Vec<u64>,
) {
    for id in contexts_to_clean.drain(..) {
        let _ = contexts.remove(&id);
    }
    if contexts.len() > 0 {
        send_to_terminal.send(t::Printout {
            verbosity: 1,
            content: format!(
                "contexts now reads: {:?}",
                contexts,
            ),
        }).await.unwrap();
    }
}

async fn get_and_send_specific_loop_message_to_process(
    awaited_message_id: u64,
    message_queue: &mut VecDeque<t::KernelMessage>,
    recv_in_process: &mut t::MessageReceiver,
    send_to_terminal: &mut t::PrintSender,
    contexts: &mut HashMap<u64, ProcessContext>,
    contexts_to_clean: &mut Vec<u64>,
) -> (t::KernelMessage, Result<Result<wit::InboundMessage, wit::UqbarError>>) {
    // clean_contexts(send_to_terminal, contexts, contexts_to_clean).await;
    loop {
        let kernel_message = recv_in_process.recv().await.unwrap();
        //  if message id matches the one we sent out
        if awaited_message_id == kernel_message.id {
            match kernel_message.message {
                Ok(ref _message) => {
                    let message = send_loop_message_to_process(
                        kernel_message,
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
                },
                Err(_) => {
                    let message = send_loop_message_to_process(
                        kernel_message,
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

        message_queue.push_back(kernel_message);
        send_to_terminal
            .send(t::Printout {
                verbosity: 1,
                content: format!("queue length now {}", message_queue.len()),
            })
            .await
            .unwrap();
    }
}

async fn get_and_send_loop_message_to_process(
    message_queue: &mut VecDeque<t::KernelMessage>,
    recv_in_process: &mut t::MessageReceiver,
    send_to_terminal: &mut t::PrintSender,
    contexts: &mut HashMap<u64, ProcessContext>,
    contexts_to_clean: &mut Vec<u64>,
) -> (t::KernelMessage, Result<Result<(wit::InboundMessage, String), wit::UqbarError>>) {
    clean_contexts(send_to_terminal, contexts, contexts_to_clean).await;
    //  TODO: dont unwrap: causes panic when Start already running process
    let kernel_message = match message_queue.pop_front() {
        Some(message_from_queue) => message_from_queue,
        None => recv_in_process.recv().await.unwrap(),
    };

    let to_loop = send_loop_message_to_process(
        kernel_message,
        send_to_terminal,
        contexts,
        contexts_to_clean,
    ).await;

    (to_loop.0, Ok(to_loop.1))
}

async fn send_loop_message_to_process(
    kernel_message: t::KernelMessage,
    send_to_terminal: &mut t::PrintSender,
    contexts: &mut HashMap<u64, ProcessContext>,
    contexts_to_clean: &mut Vec<u64>,
) -> (t::KernelMessage, Result<(wit::InboundMessage, String), wit::UqbarError>) {
    match kernel_message.message {
        Ok(ref message) => {
            let (inbound_message, context) = match message {
                t::TransitMessage::Request(r) => {
                    (
                        make_inbound_request_from_transit(r.is_expecting_response, &r.payload),
                        "".into(),
                    )
                },
                t::TransitMessage::Response(payload) => {
                    (
                        make_inbound_response_from_transit(&payload),
                        get_context(
                            &kernel_message.id,
                            send_to_terminal,
                            contexts,
                            contexts_to_clean,
                        ).await,
                    )
                }
            };

            (
                kernel_message,
                Ok((
                    inbound_message,
                    context,
                ))
            )
        },
        Err(ref e) => {
            let context = get_context(
                &kernel_message.id,
                send_to_terminal,
                contexts,
                contexts_to_clean,
            ).await;
            let e = e.clone();
            (
                kernel_message,
                Err(en_wit_error(&e, context)),
            )
        },
    }
}

async fn get_context(
    message_id: &u64,
    send_to_terminal: &mut t::PrintSender,
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
                .send(t::Printout { verbosity: 1, content: "couldn't find context for Response".into() })
                .await
                .unwrap();
            "".into()
        },
    }
}

fn make_request_id_target(
    default_id: u64,
    is_expecting_response: bool,
    prompting_message: &Option<t::KernelMessage>,
) -> (u64, t::Rsvp) {
    if is_expecting_response {
        (default_id, None)
    } else {
        //  rsvp is set if there was a Request expecting Response
        //   followed by Request(s) not expecting Response;
        //   could also be None if entire chain of Requests are
        //   not expecting Response
        match prompting_message {
            None => (rand::random(), None),
            Some(ref prompting_message) => {
                let Ok(ref pm_message) = prompting_message.message else {
                    return (rand::random(), None);
                };
                match pm_message {
                    t::TransitMessage::Response(_) => (rand::random(), None),
                    t::TransitMessage::Request(r) => {
                        if r.is_expecting_response {
                            (
                                prompting_message.id.clone(),
                                Some(r.payload.source.clone()),
                            )
                        } else {
                            (
                                prompting_message.id.clone(),
                                prompting_message.rsvp.clone(),
                            )
                        }
                    },
                }
            },
        }
    }
}

async fn make_response_id_target(
    prompting_message: &Option<t::KernelMessage>,
    contexts: &HashMap<u64, ProcessContext>,
    send_to_terminal: t::PrintSender,
) -> Option<(u64, t::ProcessReference)> {
    let Some(ref prompting_message) = prompting_message else {
        println!("need non-None prompting_message to handle Response");
        return None;
    };
    match prompting_message.message {
        Ok(ref pm_message) => {
            match pm_message {
                t::TransitMessage::Request(r) => {
                    if r.is_expecting_response {
                        Some((
                            prompting_message.id.clone(),
                            r.payload.source.clone(),
                        ))
                    } else {
                        let Some(rsvp) = prompting_message.rsvp.clone() else {
                            send_to_terminal
                                .send(t::Printout {
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
                t::TransitMessage::Response(_) => {
                    let Some(context) = contexts.get(&prompting_message.id) else {
                        send_to_terminal
                            .send(t::Printout {
                                verbosity: 0,
                                content: format!(
                                    "couldn't find context to route response via prompt: {:?}",
                                    prompting_message,
                                ),
                            })
                            .await
                            .unwrap();
                        return None;
                    };
                    let Some(ref ultimate) = context.ultimate else {
                        send_to_terminal
                            .send(t::Printout {
                                verbosity: 0,
                                content: "couldn't find ultimate cause to route response"
                                    .into(),
                            })
                            .await
                            .unwrap();
                        return None;
                    };

                    if ultimate.is_expecting_response {
                        Some((
                            ultimate.id.clone(),
                            ultimate.source.clone(),
                        ))
                    } else {
                        let Some(rsvp) = ultimate.rsvp.clone() else {
                            send_to_terminal
                                .send(t::Printout {
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
                            rsvp,
                        ))
                    }
                },
            }
        },
        Err(ref _e) => {
            let Some(context) = contexts.get(&prompting_message.id) else {
                send_to_terminal
                    .send(t::Printout {
                        verbosity: 0,
                        content: "couldn't find context to route response (err)".into(),
                    })
                    .await
                    .unwrap();
                return None;
            };
            let Some(ref ultimate) = context.ultimate else {
                send_to_terminal
                    .send(t::Printout {
                        verbosity: 0,
                        content: "couldn't find ultimate cause to route response (err)"
                            .into(),
                    })
                    .await
                    .unwrap();
                return None;
            };

            if ultimate.is_expecting_response {
                Some((
                    ultimate.id.clone(),
                    ultimate.source.clone(),
                ))
            } else {
                let Some(rsvp) = ultimate.rsvp.clone() else {
                send_to_terminal
                    .send(t::Printout {
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
    }
}

fn make_transit_request_from_outbound(
    source: t::ProcessReference,
    is_expecting_response: bool,
    payload: wit::OutboundPayload,
    prompting_message: &Option<t::KernelMessage>,
) -> Result<t::TransitMessage> {
    Ok(t::TransitMessage::Request(t::TransitRequest {
        is_expecting_response,
        payload: make_transit_payload_from_outbound(source, payload, prompting_message)?,
    }))
}

fn make_transit_payload_from_outbound(
    source: t::ProcessReference,
    payload: wit::OutboundPayload,
    prompting_message: &Option<t::KernelMessage>,
) -> Result<t::TransitPayload> {
    Ok(t::TransitPayload {
        source,
        json: payload.json,
        bytes: make_transit_payload_bytes_from_outbound(
            payload.bytes,
            prompting_message,
        )?,
    })
}

fn make_transit_payload_bytes_from_outbound(
    pb: wit::OutboundPayloadBytes,
    prompting_message: &Option<t::KernelMessage>,
) -> Result<t::TransitPayloadBytes> {
    Ok(match pb {
        wit::OutboundPayloadBytes::None => t::TransitPayloadBytes::None,
        wit::OutboundPayloadBytes::Some(bytes) => t::TransitPayloadBytes::Some(bytes),
        wit::OutboundPayloadBytes::Circumvent(bytes) => t::TransitPayloadBytes::Circumvent(bytes),
        wit::OutboundPayloadBytes::AttachCircumvented => {
            t::TransitPayloadBytes::Some(update_with_circumvented_bytes(prompting_message)?)
        },
    })
}

fn make_transit_response_from_outbound(
    source: t::ProcessReference,
    payload: wit::OutboundPayload,
    prompting_message: &Option<t::KernelMessage>,
) -> Result<t::TransitMessage> {
    Ok(t::TransitMessage::Response(make_transit_payload_from_outbound(
        source,
        payload,
        prompting_message,
    )?))
}

fn make_inbound_request_from_transit(
    is_expecting_response: bool,
    payload: &t::TransitPayload,
) -> wit::InboundMessage {
    let payload = make_inbound_payload_from_transit(payload);
    wit::InboundMessage::Request(wit::InboundRequest {
        is_expecting_response,
        payload,
    })
}

fn make_inbound_response_from_transit(payload: &t::TransitPayload) -> wit::InboundMessage {
    wit::InboundMessage::Response(make_inbound_payload_from_transit(payload))
}

fn make_inbound_payload_from_transit(
    payload: &t::TransitPayload,
) -> wit::InboundPayload {
    wit::InboundPayload {
        source: en_wit_process_reference(&payload.source),
        json: payload.json.clone(),
        bytes: match payload.bytes.clone() {
            t::TransitPayloadBytes::None => wit::InboundPayloadBytes::None,
            t::TransitPayloadBytes::Some(bytes) => wit::InboundPayloadBytes::Some(bytes),
            t::TransitPayloadBytes::Circumvent(_bytes) => wit::InboundPayloadBytes::Circumvented,
        },
    }
}

fn update_with_circumvented_bytes(
    prompting_message: &Option<t::KernelMessage>,
) -> Result<Vec<u8>> {
    let Some(pm) = prompting_message else {
        return Err(anyhow::anyhow!(""));
    };
    let Ok(ref m) = pm.message else {
        return Err(anyhow::anyhow!(""));
    };
    let bytes = match m {
        t::TransitMessage::Request(r) => {
            let t::TransitPayloadBytes::Circumvent(bytes) = r.payload.bytes.clone() else {
                return Err(anyhow::anyhow!(""));
            };
            bytes
        },
        t::TransitMessage::Response(payload) => {
            let t::TransitPayloadBytes::Circumvent(bytes) = payload.bytes.clone() else {
                return Err(anyhow::anyhow!(""));
            };
            bytes
        },
    };
    return Ok(bytes);
}

async fn insert_or_increment_context(
    is_expecting_response: bool,
    id: u64,
    context: ProcessContext,
    contexts: &mut HashMap<u64, ProcessContext>,
) {
    if is_expecting_response {
        match contexts.remove(&id) {
            Some(mut existing_context) => {
                existing_context.number_outstanding_requests += 1;
                contexts.insert(id, existing_context);
            },
            None => {
                contexts.insert(id, context);
            }
        }
    }
}

async fn decrement_context(
    id: &u64,
    send_to_terminal: &t::PrintSender,
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
        },
        None => {
            send_to_terminal.send(t::Printout {
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
    error: wit::UqbarErrorPayload,
    source: t::ProcessReference,
    send_to_loop: &t::MessageSender,
    prompting_message: &Option<t::KernelMessage>,
) -> Vec<u64> {
    let Some(ref prompting_message) = prompting_message else {
        println!("need non-None prompting_message to handle Error");
        return vec![];  //  TODO: how to handle error on error routing case?
    };

    let id = prompting_message.id.clone();
    let target = match prompting_message.message {
        Err(ref e) => e.source.clone(),
        Ok(ref m) => get_transit_message_source(m).clone(),
    };

    let kernel_message = t::KernelMessage {
        id,
        target,
        rsvp: None,
        message: Err(t::UqbarError {
            source,
            timestamp: get_current_unix_time().unwrap(),
            payload: de_wit_error_payload(&error),
        }),
    };

    send_to_loop
        .send(kernel_message)
        .await
        .unwrap();

    return vec![id];
}

async fn handle_request(
    source: t::ProcessReference,
    send_to_loop: &t::MessageSender,
    _send_to_terminal: &t::PrintSender,
    default_id: u64,
    is_expecting_response: bool,
    target: wit::ProcessReference,
    payload: wit::OutboundPayload,
    prompting_message: &Option<t::KernelMessage>,
    new_context: Option<serde_json::Value>,
    contexts: &mut HashMap<u64, ProcessContext>,
) -> Result<u64> {
    let (id, rsvp) = make_request_id_target(
        default_id,
        is_expecting_response,
        &prompting_message,
    );
    let target = de_wit_process_reference(&target);

    let transit_message = make_transit_request_from_outbound(
        source.clone(),
        is_expecting_response,
        payload,
        prompting_message,
    )?;

    let kernel_message = t::KernelMessage {
        id,
        target,
        rsvp,
        message: Ok(transit_message),
    };

    //  modify contexts
    let process_context = match prompting_message {
        Some(ref prompting_message) => {
            match prompting_message.message {
                Ok(ref prompting_message_message) => {
                    match prompting_message_message {
                        t::TransitMessage::Request(_) => {
                            //  case: prompting_message_is_expecting_response
                            //   ultimate stored for source
                            //  case: !prompting_message_is_expecting_response
                            //   ultimate stored for rsvp
                            ProcessContext::new(
                                &kernel_message,
                                Some(&prompting_message),
                                new_context,
                            )
                        },
                        t::TransitMessage::Response(_) => {
                            match contexts.get(&prompting_message.id) {
                                Some(context) => {
                                    //  ultimate is the ultimate of the prompt of Response
                                    ProcessContext::new_from_context(
                                        &kernel_message,
                                        &context.ultimate,
                                        new_context,
                                    )
                                },
                                None => {
                                    //  should this even be allowed?
                                    ProcessContext::new(
                                        &kernel_message,
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
                                &kernel_message,
                                &context.ultimate,
                                new_context,
                            )
                        },
                        None => {
                            //  should this even be allowed?
                            ProcessContext::new(
                                &kernel_message,
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
                &kernel_message,
                None,
                new_context,
            )
        },
    };
    insert_or_increment_context(
        is_expecting_response,
        kernel_message.id,
        process_context,
        contexts,
    ).await;

    send_to_loop
        .send(kernel_message)
        .await
        .unwrap();

    Ok(id)
}

async fn send_process_requests_to_loop(
    requests: Vec<wit::JoinedRequests>,
    source: t::ProcessReference,
    send_to_loop: &t::MessageSender,
    send_to_terminal: &t::PrintSender,
    prompting_message: &Option<t::KernelMessage>,
    contexts: &mut HashMap<u64, ProcessContext>,
) -> Result<Vec<u64>> {
    let mut ids: Vec<u64> = Vec::new();
    for (outbound_requests, context) in requests {
        let default_id = rand::random();
        let context = serde_json::from_str(&context).ok();
        for wit::OutboundRequest {
            is_expecting_response,
            target,
            payload,
        } in outbound_requests {
            let new_id = handle_request(
                source.clone(),
                send_to_loop,
                send_to_terminal,
                default_id,
                is_expecting_response,
                target,
                payload,
                prompting_message,
                context.clone(),
                contexts,
            ).await?;
            if !ids.contains(&new_id) {
                ids.push(new_id);
            }
        }
    }
    Ok(ids)
}

async fn send_process_response_to_loop(
    response: (wit::OutboundPayload, String),
    source: t::ProcessReference,
    send_to_loop: &t::MessageSender,
    send_to_terminal: &t::PrintSender,
    prompting_message: &Option<t::KernelMessage>,
    contexts: &mut HashMap<u64, ProcessContext>,
) -> Result<u64> {
    let payload = response.0;
    let (id, target) = match make_response_id_target(
        &prompting_message,
        contexts,
        send_to_terminal.clone(),
    ).await {
        Some(r) => r,
        None => {
            send_to_terminal
                .send(t::Printout {
                    verbosity: 1,
                    content: format!(
                        "dropping Response: {:?}; contexts: {:?}",
                        payload.json,
                        contexts,
                    ),
                })
                .await
                .unwrap();
            return Ok(0);  //  TODO: do better
        },
    };
    let rsvp = None;

    let transit_message = make_transit_response_from_outbound(
        source.clone(),
        payload,
        prompting_message,
    )?;

    let kernel_message = t::KernelMessage {
        id,
        target,
        rsvp,
        message: Ok(transit_message),
    };

    send_to_loop
        .send(kernel_message)
        .await
        .unwrap();

    return Ok(id);
}

async fn send_process_response_with_side_effect_request_to_loop(
    results: wit::ResponseWithSideEffect,
    source: t::ProcessReference,
    send_to_loop: &t::MessageSender,
    send_to_terminal: &t::PrintSender,
    prompting_message: &Option<t::KernelMessage>,
    contexts: &mut HashMap<u64, ProcessContext>,
) -> Result<Vec<u64>> {
    //  handle Response
    let (payload, _response_context) = results.response;

    let mut ids: Vec<u64> = Vec::new();

    let (id, target) = match make_response_id_target(
        &prompting_message,
        contexts,
        send_to_terminal.clone(),
    ).await {
        Some(r) => r,
        None => {
            send_to_terminal
                .send(t::Printout {
                    verbosity: 1,
                    content: format!(
                        "dropping Response: {:?}; contexts: {:?}",
                        payload.json,
                        contexts,
                    ),
                })
                .await
                .unwrap();
            return Ok(vec![]);
        },
    };
    let rsvp = None;

    let transit_message = make_transit_response_from_outbound(
        source.clone(),
        payload,
        prompting_message,
    )?;

    let kernel_message = t::KernelMessage {
        id,
        target,
        rsvp,
        message: Ok(transit_message),
    };

    send_to_loop
        .send(kernel_message)
        .await
        .unwrap();

    ids.push(id);

    //  handle side-effect Request
    let (wit::OutboundRequest { is_expecting_response, target, payload }, request_context) =
        results.request;
    let default_id = rand::random();
    let prompting_message = None;
    let request_context = Some(serde_json::to_value(request_context)?);

    ids.push(handle_request(
        source,
        send_to_loop,
        send_to_terminal,
        default_id,
        is_expecting_response,
        target,
        payload,
        &prompting_message,
        request_context,
        contexts,
    ).await?);

    return Ok(ids);
}

async fn make_process_loop(
    metadata: t::ProcessMetadata,
    send_to_loop: t::MessageSender,
    send_to_terminal: t::PrintSender,
    recv_in_process: t::MessageReceiver,
    wasm_bytes: &Vec<u8>,
    engine: &Engine,
) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
    let our_name = metadata.our.node.clone();
    let address = metadata.our.clone();
    let wasm_bytes_uri = metadata.wasm_bytes_uri.clone();
    let send_on_panic = metadata.send_on_panic.clone();

    let component = Component::new(&engine, wasm_bytes)
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
                has_already_sent: false,
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

            let is_error = match bindings.call_run_process(
                &mut store,
                &en_wit_process_address(&address),
            ).await {
                Ok(()) => false,
                Err(e) => {
                    let _ = send_to_terminal
                        .send(t::Printout {
                            verbosity: 0,
                            content: format!(
                                "mk: process {:?} ended with error: {:?}",
                                address,
                                e,
                            ),
                        })
                        .await;
                    true
                }
            };

            let our_pm = t::ProcessReference {
                node: our_name.clone(),
                identifier: t::ProcessIdentifier::Name("process_manager".into()),
            };
            let our_kernel = t::ProcessReference {
                node: our_name.clone(),
                identifier: t::ProcessIdentifier::Name("kernel".into()),  //  should this be process_name?
            };

            //  clean up process metadata & channels
            send_to_loop
                .send(t::KernelMessage {
                    id: rand::random(),
                    target: our_pm.clone(),
                    rsvp: None,
                    message: Ok(t::TransitMessage::Request(t::TransitRequest {
                        is_expecting_response: false,
                        payload: t::TransitPayload {
                            source: our_kernel.clone(),
                            json: Some(
                                serde_json::to_string(&t::ProcessManagerCommand::Stop {
                                    id: address.id.clone(),
                                }).unwrap()
                            ),
                            bytes: t::TransitPayloadBytes::None,
                        },
                    })),
                })
                .await
                .unwrap();

            if is_error {
                match send_on_panic {
                    t::SendOnPanic::None => {},
                    t::SendOnPanic::Restart => {
                        send_to_loop
                            .send(t::KernelMessage {
                                id: rand::random(),
                                target: our_pm,
                                rsvp: None,
                                message: Ok(t::TransitMessage::Request(t::TransitRequest {
                                    is_expecting_response: false,
                                    payload: t::TransitPayload {
                                        source: our_kernel,
                                        json: Some(
                                            serde_json::to_string(&t::ProcessManagerCommand::Start {
                                                name: address.name.clone(),
                                                wasm_bytes_uri,
                                                send_on_panic,
                                            }).unwrap()
                                        ),
                                        bytes: t::TransitPayloadBytes::None,
                                    },
                                })),
                            })
                            .await
                            .unwrap();
                    },
                    t::SendOnPanic::Requests(requests) => {
                        for request in requests {
                            send_to_loop
                                .send(t::KernelMessage {
                                    id: rand::random(),
                                    target: request.target,
                                    rsvp: None,
                                    message: Ok(t::TransitMessage::Request(t::TransitRequest {
                                        is_expecting_response: false,
                                        payload: t::TransitPayload {
                                            source: address_to_reference(&address),
                                            json: request.json,
                                            bytes: request.bytes,
                                        },
                                    })),
                                })
                                .await
                                .unwrap();
                        }
                    },
                }
            }
            Ok(())
        }
    )
}

async fn handle_kernel_request(
    our_name: String,
    kernel_message: t::KernelMessage,
    send_to_loop: t::MessageSender,
    send_to_terminal: t::PrintSender,
    senders: &mut Senders,
    process_handles: &mut ProcessHandles,
    names: &mut Names,
    ids: &mut Ids,
    engine: &Engine,
) {
    let Ok(message) = kernel_message.message else {
        panic!("fubar");  //  TODO
    };
    let payload = match message {
        t::TransitMessage::Request(ref request) => &request.payload,
        t::TransitMessage::Response(ref payload) => payload,
    };
    if match payload.source.identifier {
        t::ProcessIdentifier::Name(ref n) => {
            ("process_manager" != n.as_str())
            & ("kernel" != n.as_str())
        },
        t::ProcessIdentifier::Id(id) => {
            match ids.get(&id) {
                None => false,
                Some(ref n) => {
                    ("process_manager" != n.as_str())
                    & ("kernel" != n.as_str())
                },
            }
        },
    } {
        send_to_terminal
            .send(t::Printout{
                verbosity: 0,
                content: format!(
                    "kernel: rejecting kernel command not from process_manager: {}",
                    message,
                ),
            })
            .await
            .unwrap();
        return;
    }
    let Some(ref json_string) = payload.json else {
        send_to_terminal
            .send(t::Printout{
                verbosity: 0,
                content: format!(
                    "kernel: rejecting kernel command with no json payload: {}",
                    message,
                ),
            })
            .await
            .unwrap();
        return;
    };
    let kernel_request: t::KernelRequest = serde_json::from_str(json_string)
        .expect("kernel: could not parse to command");
    match kernel_request {
        t::KernelRequest::StartProcess { id, name, wasm_bytes_uri, send_on_panic } => {
            let t::TransitPayloadBytes::Some(ref wasm_bytes) = payload.bytes else {
                send_to_terminal
                    .send(t::Printout{
                        verbosity: 0,
                        content: "kernel: StartProcess requires bytes".into(),
                    })
                    .await
                    .unwrap();
                return;
            };
            let (send_to_process, recv_in_process) =
                mpsc::channel::<t::KernelMessage>(PROCESS_CHANNEL_CAPACITY);
            senders.insert(id.clone(), send_to_process);
            let metadata = t::ProcessMetadata {
                our: t::ProcessAddress {
                    node: our_name.clone(),
                    id: id.clone(),
                    name: name.clone(),
                },
                wasm_bytes_uri: wasm_bytes_uri.clone(),
                send_on_panic,
            };
            process_handles.insert(
                id.clone(),
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

            if let Some(n) = name {
                names.insert(n.clone(), id.clone());
                ids.insert(id.clone(), n.clone());
            };

            let start_completed_message = t::KernelMessage {
                id: kernel_message.id,
                target: t::ProcessReference {
                    node: our_name.clone(),
                    identifier: t::ProcessIdentifier::Name("process_manager".into()),
                },
                rsvp: None,
                message: Ok(t::TransitMessage::Response(t::TransitPayload {
                    source: t::ProcessReference {
                        node: our_name.clone(),
                        identifier: t::ProcessIdentifier::Name("kernel".into()),
                    },
                    json: Some(serde_json::to_string(&t::KernelResponse::StartProcess(metadata))
                        .unwrap()),
                    bytes: t::TransitPayloadBytes::None,
                }))
            };
            send_to_loop
                .send(start_completed_message)
                .await
                .unwrap();
        },
        t::KernelRequest::StopProcess { id } => {
            let t::TransitMessage::Request(t::TransitRequest { is_expecting_response, payload: _ })
                = message else {
            // let t::MessageType::Request(is_expecting_response) = message.content.message_type else {
                send_to_terminal
                    .send(t::Printout {
                        verbosity: 0,
                        content: "kernel: StopProcess requires Request, got Response".into()
                    })
                    .await
                    .unwrap();
                return;
            };
            let _ = senders.remove(&id);
            let process_handle = match process_handles.remove(&id) {
                Some(ph) => ph,
                None => {
                    send_to_terminal
                        .send(t::Printout {
                            verbosity: 0,
                            content: format!(
                                "kernel: no such process {} to Stop",
                                id,
                            ),
                        })
                        .await
                        .unwrap();
                    return;
                },
            };
            process_handle.abort();
            if let Some(n) = ids.remove(&id) {
                names.remove(&n);
            };

            if !is_expecting_response {
                return;
            }
            let json_payload = serde_json::to_string(&t::KernelResponse::StopProcess { id })
                .unwrap();

            let stop_completed_message = t::KernelMessage {
                id: kernel_message.id,
                target: t::ProcessReference {
                    node: our_name.clone(),
                    identifier: t::ProcessIdentifier::Name("process_manager".into()),
                },
                rsvp: None,
                message: Ok(t::TransitMessage::Response(t::TransitPayload {
                    source: t::ProcessReference {
                        node: our_name.clone(),
                        identifier: t::ProcessIdentifier::Name("kernel".into()),
                    },
                    json: Some(json_payload),
                    bytes: t::TransitPayloadBytes::None,
                }))
            };
            send_to_loop
                .send(stop_completed_message)
                .await
                .unwrap();
        },
        t::KernelRequest::RegisterProcess { id, name } => {
            if ids.contains_key(&id) {
                send_to_terminal
                    .send(t::Printout {
                        verbosity: 0,
                        content: format!(
                            "kernel: RegisterProcess id {} already registered",
                            id,
                        ),
                    })
                    .await
                    .unwrap();
                return;
            }
            if names.contains_key(&name) {
                send_to_terminal
                    .send(t::Printout {
                        verbosity: 0,
                        content: format!(
                            "kernel: RegisterProcess name {} already registered",
                            name,
                        ),
                    })
                    .await
                    .unwrap();
                return;
            }

            ids.insert(id.clone(), name.clone());
            names.insert(name.clone(), id.clone());
        },
        t::KernelRequest::UnregisterProcess { id } => {
            match ids.remove(&id) {
                Some(name) => {
                    names.remove(&name);
                },
                None => {
                    send_to_terminal
                        .send(t::Printout {
                            verbosity: 0,
                            content: format!(
                                "kernel: UnregisterProcess id {} not registered",
                                id,
                            ),
                        })
                        .await
                        .unwrap();
                    return;
                },
            }
        },
    }
}

async fn make_event_loop(
    our_name: String,
    mut recv_in_loop: t::MessageReceiver,
    mut recv_debug_in_loop: t::DebugReceiver,
    send_to_loop: t::MessageSender,
    send_to_net: t::MessageSender,
    send_to_fs: t::MessageSender,
    send_to_lfs: t::MessageSender,
    send_to_http_server: t::MessageSender,
    send_to_http_client: t::MessageSender,
    send_to_terminal: t::PrintSender,
    engine: Engine,
) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
    Box::pin(
        async move {
            let mut senders: Senders = HashMap::new();
            senders.insert(t::FILESYSTEM_ID, send_to_fs);
            senders.insert(t::HTTP_SERVER_ID, send_to_http_server);
            senders.insert(t::HTTP_CLIENT_ID, send_to_http_client);
            senders.insert(t::LFS_ID, send_to_lfs);

            let mut names: Names = HashMap::new();
            names.insert("kernel".into(), t::KERNEL_ID);
            names.insert("filesystem".into(), t::FILESYSTEM_ID);
            names.insert("http_server".into(), t::HTTP_SERVER_ID);
            names.insert("http_client".into(), t::HTTP_CLIENT_ID);
            names.insert("lfs".into(), t::LFS_ID);

            let mut ids: Ids = HashMap::new();
            ids.insert(t::KERNEL_ID, "kernel".into());
            ids.insert(t::FILESYSTEM_ID, "filesystem".into());
            ids.insert(t::HTTP_SERVER_ID, "http_server".into());
            ids.insert(t::HTTP_CLIENT_ID, "http_client".into());
            ids.insert(t::LFS_ID, "lfs".into());

            let mut process_handles: ProcessHandles = HashMap::new();
            let mut is_debug = false;
            loop {
                tokio::select! {
                    debug = recv_debug_in_loop.recv() => {
                        if let Some(t::DebugCommand::Toggle) = debug {
                            is_debug = !is_debug;
                        }
                    },
                    kernel_message = recv_in_loop.recv() => {
                        while is_debug {
                            let debug = recv_debug_in_loop.recv().await.unwrap();
                            match debug {
                                t::DebugCommand::Toggle => is_debug = !is_debug,
                                t::DebugCommand::Step => break,
                            }
                        }

                        let Some(kernel_message) = kernel_message else {
                            send_to_terminal.send(t::Printout {
                                    verbosity: 1,
                                    content: "event loop: got None for message".into(),
                                }
                            ).await.unwrap();
                            continue;
                        };
                        send_to_terminal.send(
                            t::Printout {
                                verbosity: 1,
                                content: format!("event loop: got message: {}", kernel_message)
                            }
                        ).await.unwrap();
                        if our_name != kernel_message.target.node {
                            match send_to_net.send(kernel_message).await {
                                Ok(()) => {
                                    send_to_terminal
                                        .send(t::Printout {
                                            verbosity: 1,
                                            content: "event loop: message sent to network".into(),
                                        })
                                        .await
                                        .unwrap();
                                }
                                Err(e) => {
                                    send_to_terminal
                                        .send(t::Printout {
                                            verbosity: 0,
                                            content: format!("event loop: message to network failed: {}", e),
                                        })
                                        .await
                                        .unwrap();
                                }
                            }
                        } else {
                            let (id, maybe_name) = match kernel_message.target.identifier {
                                t::ProcessIdentifier::Id(id) => {
                                    (id.clone(), ids.get(&id))
                                },
                                t::ProcessIdentifier::Name(ref name) => {
                                    match names.get(name) {
                                        Some(id) => (id.clone(), Some(name)),
                                        None => {
                                            send_to_terminal
                                                .send(t::Printout {
                                                    verbosity: 0,
                                                    content: format!(
                                                        "event loop: no id corresponding to name given in Message {}",
                                                        kernel_message,
                                                    ),
                                                })
                                                .await
                                                .unwrap();
                                            continue;
                                        },
                                    }
                                },
                            };
                            if Some(&"kernel".to_string()) == maybe_name {
                                handle_kernel_request(
                                    our_name.clone(),
                                    kernel_message,
                                    send_to_loop.clone(),
                                    send_to_terminal.clone(),
                                    &mut senders,
                                    &mut process_handles,
                                    &mut names,
                                    &mut ids,
                                    &engine,
                                ).await;
                            //  XX temporary branch to assist in pure networking debugging
                            //  can be removed when ws WASM module is ready
                            } else if Some(&"net".to_string()) == maybe_name {
                                let _ = send_to_net.send(kernel_message).await;
                            } else {
                                //  pass message to appropriate runtime/process
                                match senders.get(&id) {
                                    Some(sender) => {
                                        let _result = sender
                                            .send(kernel_message)
                                            .await;
                                    }
                                    None => {
                                        send_to_terminal
                                            .send(t::Printout {
                                                verbosity: 0,
                                                content: format!(
                                                    "event loop: don't have {:?}, {:?} amongst registered processes: {:?}",
                                                    id,
                                                    maybe_name,
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
    our: t::Identity,
    send_to_loop: t::MessageSender,
    send_to_terminal: t::PrintSender,
    recv_in_loop: t::MessageReceiver,
    recv_debug_in_loop: t::DebugReceiver,
    send_to_wss: t::MessageSender,
    send_to_fs: t::MessageSender,
    send_to_lfs: t::MessageSender,
    send_to_http_server: t::MessageSender,
    send_to_http_client: t::MessageSender,
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
