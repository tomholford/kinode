use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use wasmtime::component::*;
use wasmtime::{Config, Engine, Store, WasmBacktraceDetails};

use wasmtime_wasi::preview2::{DirPerms, FilePerms, Table, WasiCtx, WasiCtxBuilder, WasiView};

use crate::types as t;
//  WIT errors when `use`ing interface unless we import this and implement Host for Process below
use crate::kernel::component::uq_process::types as wit;
use crate::kernel::component::uq_process::types::Host;

mod utils;
use crate::kernel::utils::*;

bindgen!({
    path: "wit",
    world: "uq-process",
    async: true,
});
const PROCESS_CHANNEL_CAPACITY: usize = 100;

pub type ProcessMessageSender =
    tokio::sync::mpsc::Sender<Result<t::KernelMessage, t::WrappedNetworkError>>;
pub type ProcessMessageReceiver =
    tokio::sync::mpsc::Receiver<Result<t::KernelMessage, t::WrappedNetworkError>>;

struct Process {
    metadata: t::ProcessMetadata,
    recv_in_process: ProcessMessageReceiver,
    send_to_loop: t::MessageSender,
    send_to_terminal: t::PrintSender,
    prompting_message: Option<t::KernelMessage>,
    contexts: HashMap<u64, ProcessContext>,
    message_queue: VecDeque<Result<t::KernelMessage, t::WrappedNetworkError>>,
}

#[derive(Clone, Debug)]
struct ProcessContext {
    // store ultimate in order to set prompting message if needed
    prompting_message: Option<t::KernelMessage>,
    // can be empty if a request doesn't set context, but still needs to inherit
    context: Option<t::Context>,
}

struct ProcessWasi {
    process: Process,
    table: Table,
    wasi: WasiCtx,
}

#[derive(Serialize, Deserialize, Debug)]
struct StartProcessMetadata {
    source: t::Address,
    name: Option<String>,
    wasm_bytes_handle: u128,
    on_panic: t::OnPanic,
}

//  live in event loop
type Senders = HashMap<t::ProcessId, ProcessSender>;
type ProcessHandles = HashMap<t::ProcessId, JoinHandle<Result<()>>>;

enum ProcessSender {
    Runtime(t::MessageSender),
    Userspace(ProcessMessageSender),
}

impl Host for ProcessWasi {}

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

///
/// intercept wasi random
///

#[async_trait::async_trait]
impl wasi::random::insecure::Host for ProcessWasi {
    async fn get_insecure_random_bytes(&mut self, len: u64) -> Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(len as usize);
        for _ in 0..len {
            bytes.push(rand::random());
        }
        Ok(bytes)
    }

    async fn get_insecure_random_u64(&mut self) -> Result<u64> {
        Ok(rand::random())
    }
}

#[async_trait::async_trait]
impl wasi::random::insecure_seed::Host for ProcessWasi {
    async fn insecure_seed(&mut self) -> Result<(u64, u64)> {
        Ok((rand::random(), rand::random()))
    }
}

#[async_trait::async_trait]
impl wasi::random::random::Host for ProcessWasi {
    async fn get_random_bytes(&mut self, len: u64) -> Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(len as usize);
        getrandom::getrandom(&mut bytes[..])?;
        Ok(bytes)
    }

    async fn get_random_u64(&mut self) -> Result<u64> {
        let mut bytes = Vec::with_capacity(8);
        getrandom::getrandom(&mut bytes[..])?;

        let mut number = 0u64;
        for (i, &byte) in bytes.iter().enumerate() {
            number |= (byte as u64) << (i * 8);
        }
        Ok(number)
    }
}

///
/// create the process API. this is where the functions that a process can use live.
///
#[async_trait::async_trait]
impl UqProcessImports for ProcessWasi {
    //
    // system utils:
    //f
    async fn print_to_terminal(&mut self, verbosity: u8, content: String) -> Result<()> {
        match self
            .process
            .send_to_terminal
            .send(t::Printout { verbosity, content })
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => Err(anyhow::anyhow!("fatal: couldn't send to terminal: {:?}", e)),
        }
    }

    async fn get_unix_time(&mut self) -> Result<u64> {
        match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(t) => Ok(t.as_secs()),
            Err(e) => Err(e.into()),
        }
    }

    async fn get_eth_block(&mut self) -> Result<u64> {
        // TODO connect to eth RPC
        unimplemented!()
    }

    //
    // process management:
    //

    async fn set_on_panic(&mut self, on_panic: wit::OnPanic) -> Result<()> {
        let on_panic = match on_panic {
            wit::OnPanic::None => t::OnPanic::None,
            wit::OnPanic::Restart => t::OnPanic::Restart,
            wit::OnPanic::Requests(reqs) => t::OnPanic::Requests(
                reqs.into_iter()
                    .map(|(addr, req, payload)| {
                        (
                            de_wit_address(addr),
                            de_wit_request(req),
                            de_wit_payload(payload),
                        )
                    })
                    .collect(),
            ),
        };

        self.process.metadata.on_panic = on_panic;
        Ok(())
    }

    //
    // message I/O:
    //

    /// from a process: receive the next incoming message. will wait async until a message is received.
    /// the incoming message can be a Request or a Response, or an Error of the Network variety.
    async fn receive(
        &mut self,
    ) -> Result<Result<(wit::Address, wit::Message), (wit::NetworkError, Option<wit::Context>)>>
    {
        Ok(self.process.get_next_message_for_process().await)
    }

    /// from a process: grab the payload part of the current prompting message.
    /// if the prompting message did not have a payload, will return None.
    /// will also return None if there is no prompting message.
    async fn get_payload(&mut self) -> Result<Option<wit::Payload>> {
        match self.process.prompting_message.clone() {
            Some(km) => Ok(en_wit_payload(km.payload)),
            None => Ok(None),
        }
    }

    async fn send_request(
        &mut self,
        target: wit::Address,
        request: wit::Request,
        context: Option<wit::Context>,
        payload: Option<wit::Payload>,
    ) -> Result<()> {
        let id = self
            .process
            .handle_request(target, request, context, payload)
            .await;
        match id {
            Ok(_id) => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn send_requests(
        &mut self,
        requests: Vec<(
            wit::Address,
            wit::Request,
            Option<wit::Context>,
            Option<wit::Payload>,
        )>,
    ) -> Result<()> {
        for request in requests {
            let id = self
                .process
                .handle_request(request.0, request.1, request.2, request.3)
                .await;
            match id {
                Ok(_id) => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    async fn send_response(
        &mut self,
        response: wit::Response,
        payload: Option<wit::Payload>,
    ) -> Result<()> {
        self.process.send_response(response, payload).await;
        Ok(())
    }

    async fn send_error(&mut self, error: wit::UqbarError) -> Result<()> {
        self.process.send_error(error).await;
        Ok(())
    }

    async fn send_and_await_response(
        &mut self,
        target: wit::Address,
        request: wit::Request,
        context: Option<wit::Context>,
        payload: Option<wit::Payload>,
    ) -> Result<Result<(wit::Address, wit::Message), (wit::NetworkError, Option<wit::Context>)>>
    {
        let id = self
            .process
            .handle_request(target, request, context, payload)
            .await;
        match id {
            Ok(id) => match self.process.get_specific_message_for_process(id).await {
                Ok((address, wit::Message::Response(response))) => {
                    Ok(Ok((address, wit::Message::Response(response))))
                }
                Ok((_address, wit::Message::Request(_))) => {
                    // this is an error
                    Err(anyhow::anyhow!(
                        "fatal: received Request instead of Response"
                    ))
                }
                Err((net_err, context)) => Ok(Err((net_err, context))),
            },
            Err(e) => Err(e),
        }
    }
}

impl Process {
    /// save a context for a given request.
    async fn save_context(
        &mut self,
        request_id: u64,
        request: t::Request,
        context: Option<t::Context>,
    ) {
        self.contexts.insert(
            request_id,
            ProcessContext {
                prompting_message: if self.prompting_message.is_some() {
                    if request.inherit {
                        self.prompting_message.clone()
                    } else {
                        None
                    }
                } else {
                    None
                },
                context,
            },
        );
    }

    /// Ingest latest message directed to this process, and mark it as the prompting message.
    /// If there is no message in the queue, wait async until one is received.
    /// The message will only be saved as the prompting-message if it's a Request.
    async fn get_next_message_for_process(
        &mut self,
    ) -> Result<(wit::Address, wit::Message), (wit::NetworkError, Option<wit::Context>)> {
        let res = match self.message_queue.pop_front() {
            Some(message_from_queue) => message_from_queue,
            None => self.recv_in_process.recv().await.unwrap(),
        };
        match res {
            Ok(km) => match self.contexts.remove(&km.id) {
                None => {
                    self.prompting_message = Some(km.clone());
                    Ok(self.kernel_message_to_process_receive(None, km))
                }
                Some(context) => {
                    self.prompting_message = match context.prompting_message {
                        None => Some(km.clone()),
                        Some(prompting_message) => Some(prompting_message),
                    };
                    Ok(self.kernel_message_to_process_receive(context.context, km))
                }
            },
            Err(e) => match self.contexts.remove(&e.id) {
                None => Err((en_wit_network_error(e.error), None)),
                Some(context) => {
                    self.prompting_message = context.prompting_message;
                    Err((en_wit_network_error(e.error), context.context))
                }
            },
        }
    }

    /// instead of ingesting latest, wait for a specific ID and queue all others
    async fn get_specific_message_for_process(
        &mut self,
        awaited_message_id: u64,
    ) -> Result<(wit::Address, wit::Message), (wit::NetworkError, Option<wit::Context>)> {
        loop {
            let res = match self.message_queue.pop_front() {
                Some(message_from_queue) => message_from_queue,
                None => self.recv_in_process.recv().await.unwrap(),
            };
            match res {
                Ok(km) => {
                    if km.id == awaited_message_id {
                        match self.contexts.remove(&km.id) {
                            None => {
                                self.prompting_message = Some(km.clone());
                                return Ok(self.kernel_message_to_process_receive(None, km));
                            }
                            Some(context) => {
                                self.prompting_message = match context.prompting_message {
                                    None => Some(km.clone()),
                                    Some(prompting_message) => Some(prompting_message),
                                };
                                return Ok(
                                    self.kernel_message_to_process_receive(context.context, km)
                                );
                            }
                        }
                    } else {
                        self.message_queue.push_back(Ok(km));
                        continue;
                    }
                }
                Err(e) => {
                    if e.id == awaited_message_id {
                        match self.contexts.remove(&e.id) {
                            None => return Err((en_wit_network_error(e.error), None)),
                            Some(context) => {
                                self.prompting_message = context.prompting_message;
                                return Err((en_wit_network_error(e.error), context.context));
                            }
                        }
                    } else {
                        self.message_queue.push_back(Err(e));
                        continue;
                    }
                }
            }
        }
    }

    /// convert a message from the main event loop into a result for the process to receive
    /// if the message is a response or error, get context if we have one
    fn kernel_message_to_process_receive(
        &mut self,
        context: Option<t::Context>,
        km: t::KernelMessage,
    ) -> (wit::Address, wit::Message) {
        // note: the context in the KernelMessage is not actually the one we want:
        // (in fact it should be None, possibly always)
        // we need to get *our* context for this message id
        (
            en_wit_address(km.source),
            match km.message {
                t::Message::Request(request) => wit::Message::Request(en_wit_request(request)),
                t::Message::Response((Ok(response), _context)) => {
                    wit::Message::Response((Ok(en_wit_response(response)), context))
                }
                t::Message::Response((Err(error), _context)) => {
                    wit::Message::Response((Err(en_wit_uqbar_error(error)), context))
                }
            },
        )
    }

    /// Given the current process state, return the id and target that
    /// a response it emits should have. This takes into
    /// account the `rsvp` of the prompting message, if any.
    async fn make_response_id_target(&self) -> Option<(u64, t::Address)> {
        let Some(ref prompting_message) = self.prompting_message else {
            println!("need non-None prompting_message to handle Response");
            return None;
        };
        match &prompting_message.rsvp {
            None => {
                println!("prompting_message has no rsvp, no bueno");
                return None;
            }
            Some(address) => Some((prompting_message.id, address.clone())),
        }
    }

    /// takes Request generated by a process and sends it to the main event loop.
    /// should never fail.
    async fn handle_request(
        &mut self,
        target: wit::Address,
        request: wit::Request,
        new_context: Option<wit::Context>,
        payload: Option<wit::Payload>,
    ) -> Result<u64> {
        let source = self.metadata.our.clone();
        // if request chooses to inherit context, match id to prompting_message
        // otherwise, id is generated randomly
        let request_id: u64 =
            if request.inherit && !request.expects_response && self.prompting_message.is_some() {
                self.prompting_message.as_ref().unwrap().id
            } else {
                loop {
                    let id = rand::random();
                    if !self.contexts.contains_key(&id) {
                        break id;
                    }
                }
            };

        // rsvp is set if there was a Request expecting Response
        // followed by inheriting Request(s) not expecting Response;
        // this is done such that the ultimate request handler knows that,
        // in fact, a Response *is* expected.
        // could also be None if entire chain of Requests are
        // not expecting Response
        let kernel_message = t::KernelMessage {
            id: request_id,
            source: source.clone(),
            target: de_wit_address(target),
            rsvp: match (
                request.inherit,
                request.expects_response,
                &self.prompting_message,
            ) {
                // this request inherits, but has no rsvp, so itself receives any response
                (true, true, None) => Some(source),
                // this request wants a response, which overrides any prompting message
                (false, true, _) => Some(source),
                // this request inherits, so regardless of whether it expects response,
                // response will be routed to prompting message
                (true, _, Some(ref prompt)) => prompt.rsvp.clone(),
                // this request doesn't inherit, and doesn't itself want a response
                (false, false, _) => None,
                // no rsvp because neither prompting message nor this request wants a response
                (_, false, None) => None,
            },
            message: t::Message::Request(de_wit_request(request.clone())),
            payload: de_wit_payload(payload),
        };

        // modify the process' context map as needed.
        // if there is a prompting message, we need to store the ultimate
        // even if there is no new context string.
        self.save_context(kernel_message.id, de_wit_request(request), new_context)
            .await;

        self.send_to_loop
            .send(kernel_message)
            .await
            .expect("fatal: kernel couldn't send request");

        Ok(request_id)
    }

    /// takes Response generated by a process and sends it to the main event loop.
    async fn send_response(&mut self, response: wit::Response, payload: Option<wit::Payload>) {
        let (id, target) = match self.make_response_id_target().await {
            Some(r) => r,
            None => {
                self.send_to_terminal
                    .send(t::Printout {
                        verbosity: 1,
                        content: format!("dropping Response",),
                    })
                    .await
                    .unwrap();
                return;
            }
        };

        self.send_to_loop
            .send(t::KernelMessage {
                id,
                source: self.metadata.our.clone(),
                target,
                rsvp: None,
                message: t::Message::Response((
                    Ok(de_wit_response(response)),
                    // the context will be set by the process receiving this Response.
                    None,
                )),
                payload: de_wit_payload(payload),
            })
            .await
            .unwrap();
    }

    /// take an UqbarError produced as a response to a request and turn it into
    /// a KernelMessage containing a Response, then send that to the main event loop.
    /// it will be sent to the prompting message. if there is no prompting message,
    /// the error will be thrown away!
    async fn send_error(&mut self, error: wit::UqbarError) {
        let Some(ref prompting_message) = self.prompting_message else {
            return
        };

        let kernel_message = t::KernelMessage {
            id: prompting_message.id,
            source: self.metadata.our.clone(),
            target: prompting_message.rsvp.clone().unwrap(),
            rsvp: None,
            message: t::Message::Response((Err(de_wit_uqbar_error(error)), None)),
            payload: None,
        };

        self.send_to_loop.send(kernel_message).await.unwrap();
    }
}

/// create a specific process, and generate a task that will run it.
async fn make_process_loop(
    home_directory_path: String,
    metadata: t::ProcessMetadata,
    send_to_loop: t::MessageSender,
    send_to_terminal: t::PrintSender,
    recv_in_process: ProcessMessageReceiver,
    wasm_bytes: &Vec<u8>,
    engine: &Engine,
) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
    let our = metadata.our.clone();
    let wasm_bytes_handle = metadata.wasm_bytes_handle.clone();
    let on_panic = metadata.on_panic.clone();

    // let dir = std::env::current_dir().unwrap();
    let dir = cap_std::fs::Dir::open_ambient_dir(home_directory_path, cap_std::ambient_authority())
        .unwrap();

    let component =
        Component::new(&engine, wasm_bytes).expect("make_process_loop: couldn't read file");

    let mut linker = Linker::new(&engine);
    UqProcess::add_to_linker(&mut linker, |state: &mut ProcessWasi| state).unwrap();

    let mut table = Table::new();
    let wasi = WasiCtxBuilder::new()
        .push_preopened_dir(dir, DirPerms::all(), FilePerms::all(), &"")
        .build(&mut table)
        .unwrap();

    // wasmtime_wasi::preview2::command::add_to_linker(&mut linker).unwrap();
    wasmtime_wasi::preview2::bindings::clocks::wall_clock::add_to_linker(&mut linker, |t| t)
        .unwrap();
    wasmtime_wasi::preview2::bindings::clocks::monotonic_clock::add_to_linker(&mut linker, |t| t)
        .unwrap();
    wasmtime_wasi::preview2::bindings::clocks::timezone::add_to_linker(&mut linker, |t| t).unwrap();
    wasmtime_wasi::preview2::bindings::filesystem::filesystem::add_to_linker(&mut linker, |t| t)
        .unwrap();
    wasmtime_wasi::preview2::bindings::poll::poll::add_to_linker(&mut linker, |t| t).unwrap();
    wasmtime_wasi::preview2::bindings::io::streams::add_to_linker(&mut linker, |t| t).unwrap();
    // wasmtime_wasi::preview2::bindings::random::random::add_to_linker(&mut linker, |t| t).unwrap();
    wasmtime_wasi::preview2::bindings::cli_base::exit::add_to_linker(&mut linker, |t| t).unwrap();
    wasmtime_wasi::preview2::bindings::cli_base::environment::add_to_linker(&mut linker, |t| t)
        .unwrap();
    wasmtime_wasi::preview2::bindings::cli_base::preopens::add_to_linker(&mut linker, |t| t)
        .unwrap();
    wasmtime_wasi::preview2::bindings::cli_base::stdin::add_to_linker(&mut linker, |t| t).unwrap();
    wasmtime_wasi::preview2::bindings::cli_base::stdout::add_to_linker(&mut linker, |t| t).unwrap();
    wasmtime_wasi::preview2::bindings::cli_base::stderr::add_to_linker(&mut linker, |t| t).unwrap();
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

    Box::pin(async move {
        let (bindings, _bindings) =
            match UqProcess::instantiate_async(&mut store, &component, &linker).await {
                Ok(b) => b,
                Err(e) => {
                    let _ = send_to_terminal
                        .send(t::Printout {
                            verbosity: 0,
                            content: format!(
                                "mk: process {:?} failed to instantiate: {:?}",
                                our.process, e,
                            ),
                        })
                        .await;
                    return Err(e);
                }
            };

        // the process will run until it returns from init()
        let is_error = match bindings
            .call_init(&mut store, &en_wit_address(our.clone()))
            .await
        {
            Ok(()) => false,
            Err(e) => {
                let _ =
                    send_to_terminal
                        .send(t::Printout {
                            verbosity: 0,
                            content: format!(
                                "mk: process {:?} ended with error: {:?}",
                                our.process, e,
                            ),
                        })
                        .await;
                true
            }
        };

        // the process has completed, perform cleanup
        let our_kernel = t::Address {
            node: our.node.clone(),
            process: t::ProcessId::Name("kernel".into()),
        };

        if is_error {
            // fulfill the designated OnPanic behavior
            match on_panic {
                t::OnPanic::None => {}
                // if restart, tell ourselves to init the app again
                t::OnPanic::Restart => {
                    send_to_loop
                        .send(t::KernelMessage {
                            id: rand::random(),
                            source: our_kernel.clone(),
                            target: our_kernel.clone(),
                            rsvp: None,
                            message: t::Message::Request(t::Request {
                                inherit: false,
                                expects_response: false,
                                ipc: Some(
                                    serde_json::to_string(&t::KernelCommand::StartProcess {
                                        name: match &our.process {
                                            t::ProcessId::Name(name) => Some(name.into()),
                                            t::ProcessId::Id(_) => None,
                                        },
                                        wasm_bytes_handle,
                                        on_panic,
                                    })
                                    .unwrap(),
                                ),
                                metadata: None,
                            }),
                            payload: None,
                        })
                        .await
                        .unwrap();
                }
                // if requests, fire them
                t::OnPanic::Requests(requests) => {
                    for (address, mut request, payload) in requests {
                        request.expects_response = false;
                        send_to_loop
                            .send(t::KernelMessage {
                                id: rand::random(),
                                source: our.clone(),
                                target: address,
                                rsvp: None,
                                message: t::Message::Request(request),
                                payload,
                            })
                            .await
                            .unwrap();
                    }
                }
            }
        }

        // always send message to tell main kernel loop to remove handler
        send_to_loop
            .send(t::KernelMessage {
                id: rand::random(),
                source: our_kernel.clone(),
                target: our_kernel.clone(),
                rsvp: None,
                message: t::Message::Request(t::Request {
                    inherit: false,
                    expects_response: false,
                    ipc: Some(
                        serde_json::to_string(&t::KernelCommand::KillProcess(our.process.clone()))
                            .unwrap(),
                    ),
                    metadata: None,
                }),
                payload: None,
            })
            .await
            .unwrap();
        Ok(())
    })
}

/// handle messages sent directly to kernel. source is always our own node.
async fn handle_kernel_request(
    our_name: String,
    km: t::KernelMessage,
    send_to_loop: t::MessageSender,
    send_to_terminal: t::PrintSender,
    senders: &mut Senders,
    process_handles: &mut ProcessHandles,
) {
    // TODO capabilities-based security

    let t::Message::Request(request) = km.message else { return };
    let command: t::KernelCommand = match serde_json::from_str(&request.ipc.unwrap_or_default()) {
        Err(e) => {
            send_to_terminal
                .send(t::Printout {
                    verbosity: 1,
                    content: format!("kernel: couldn't parse command: {:?}", e),
                })
                .await
                .unwrap();
            return;
        }
        Ok(c) => c,
    };
    match command {
        //
        // initialize a new process. this is the only way to create a new process.
        // this sends a read request to filesystem, when response is received,
        // the process is spawned
        //
        t::KernelCommand::StartProcess {
            name,
            wasm_bytes_handle,
            on_panic,
        } => send_to_loop
            .send(t::KernelMessage {
                id: km.id,
                source: t::Address {
                    node: our_name.clone(),
                    process: t::ProcessId::Name("kernel".into()),
                },
                target: t::Address {
                    node: our_name.clone(),
                    process: t::ProcessId::Name("lfs".into()),
                },
                rsvp: None,
                message: t::Message::Request(t::Request {
                    inherit: true,
                    expects_response: true,
                    ipc: Some(
                        serde_json::to_string(&t::FsAction::Read(wasm_bytes_handle))
                        .unwrap(),
                    ),
                    // TODO find a better way if possible: keeping process metadata
                    // in request/response roundtrip because kernel itself doesn't
                    // have contexts to rely on..
                    // filesystem has to give this back to us.
                    metadata: Some(
                        serde_json::to_string(&StartProcessMetadata {
                            source: km.source,
                            name,
                            wasm_bytes_handle,
                            on_panic,
                        })
                        .unwrap(),
                    ),
                }),
                payload: None,
            })
            .await
            .unwrap(),
        t::KernelCommand::KillProcess(process_id) => {
            // brutal and savage killing: aborting the task.
            // do not do this to a process if you don't want to risk
            // dropped messages / un-replied-to-requests
            let _ = senders.remove(&process_id);
            let process_handle = match process_handles.remove(&process_id) {
                Some(ph) => ph,
                None => {
                    send_to_terminal
                        .send(t::Printout {
                            verbosity: 0,
                            content: format!("kernel: no such process {:?} to Stop", process_id),
                        })
                        .await
                        .unwrap();
                    return;
                }
            };
            process_handle.abort();

            if !request.expects_response {
                return;
            }

            send_to_loop
                .send(t::KernelMessage {
                    id: km.id,
                    source: t::Address {
                        node: our_name.clone(),
                        process: t::ProcessId::Name("kernel".into()),
                    },
                    target: km.source,
                    rsvp: None,
                    message: t::Message::Response((
                        Ok(t::Response {
                            ipc: Some(
                                serde_json::to_string(&t::KernelResponse::KilledProcess(
                                    process_id,
                                ))
                                .unwrap(),
                            ),
                            metadata: None,
                        }),
                        None,
                    )),
                    payload: None,
                })
                .await
                .unwrap();
        }
    }
}

/// currently, the kernel only receives one class of response, file-read responses
/// from the filesystem module. it uses these to get wasm bytes of a process and
/// start that process.
async fn handle_kernel_response(
    our_name: String,
    home_directory_path: String,
    km: t::KernelMessage,
    send_to_loop: t::MessageSender,
    send_to_terminal: t::PrintSender,
    senders: &mut Senders,
    process_handles: &mut ProcessHandles,
    engine: &Engine,
) {
    let t::Message::Response((Ok(ref response), _)) = km.message else {
        let _ = send_to_terminal.send(t::Printout {
            verbosity: 1,
            content: "kernel: got weird Response".into(),
        }).await;
        return;
    };

    // ignore responses that aren't filesystem responses
    if km.source.process != t::ProcessId::Name("lfs".into()) {
        return;
    }

    let Some(ref metadata) = response.metadata else {
        //  let _ = send_to_terminal.send(t::Printout {
        //      verbosity: 1,
        //      content: "kernel: response missing metadata".into(),
        //  }).await;
        return;
    };

    let meta: StartProcessMetadata = match serde_json::from_str(&metadata) {
        Err(_) => {
            let _ = send_to_terminal
                .send(t::Printout {
                    verbosity: 1,
                    content: "kernel: got weird metadata from filesystem".into(),
                })
                .await;
            return;
        }
        Ok(m) => m,
    };

    let Some(ref payload) = km.payload else {
        send_to_terminal
            .send(t::Printout{
                verbosity: 0,
                content: "kernel: process startup requires bytes".into(),
            })
            .await
            .unwrap();
        return;
    };

    start_process(
        our_name,
        home_directory_path,
        km.id,
        &payload.bytes,
        send_to_loop,
        send_to_terminal,
        senders,
        process_handles,
        engine,
        meta,
    )
    .await;
}


async fn start_process(
    our_name: String,
    home_directory_path: String,
    km_id: u64,
    km_payload_bytes: &Vec<u8>,
    send_to_loop: t::MessageSender,
    send_to_terminal: t::PrintSender,
    senders: &mut Senders,
    process_handles: &mut ProcessHandles,
    engine: &Engine,
    process_metadata: StartProcessMetadata,
) {
    let (send_to_process, recv_in_process) =
        mpsc::channel::<Result<t::KernelMessage, t::WrappedNetworkError>>(PROCESS_CHANNEL_CAPACITY);
    let process_id = if let Some(name) = process_metadata.name {
        if senders.contains_key(&t::ProcessId::Name(name.clone())) {
            // TODO: make a Response to indicate failure?
            send_to_terminal
                .send(t::Printout {
                    verbosity: 0,
                    content: format!("kernel: process named {} already exists", name),
                })
                .await
                .unwrap();
            return;
        } else {
            t::ProcessId::Name(name)
        }
    } else {
        loop {
            // lol
            let id: u64 = rand::random();
            if senders.contains_key(&t::ProcessId::Id(id)) {
                continue;
            } else {
                break t::ProcessId::Id(id);
            }
        }
    };
    senders.insert(
        process_id.clone(),
        ProcessSender::Userspace(send_to_process),
    );
    let metadata = t::ProcessMetadata {
        our: t::Address {
            node: our_name.clone(),
            process: process_id.clone(),
        },
        wasm_bytes_handle: process_metadata.wasm_bytes_handle.clone(),
        on_panic: process_metadata.on_panic,
    };
    process_handles.insert(
        process_id,
        tokio::spawn(
            make_process_loop(
                home_directory_path,
                metadata.clone(),
                send_to_loop.clone(),
                send_to_terminal.clone(),
                recv_in_process,
                &km_payload_bytes,
                engine,
            )
            .await,
        ),
    );

    send_to_loop
        .send(t::KernelMessage {
            id: km_id,
            source: t::Address {
                node: our_name.clone(),
                process: t::ProcessId::Name("kernel".into()),
            },
            target: process_metadata.source,
            rsvp: None,
            message: t::Message::Response((
                Ok(t::Response {
                    ipc: Some(
                        serde_json::to_string(&t::KernelResponse::StartedProcess(metadata))
                            .unwrap(),
                    ),
                    metadata: None,
                }),
                None,
            )),
            payload: None,
        })
        .await
        .unwrap();
}

/// process event loop. allows WASM processes to send messages to various runtime modules.
/// if this dies, it's over
async fn make_event_loop(
    our_name: String,
    home_directory_path: String,
    process_map: HashMap<t::ProcessId, u128>,
    mut recv_in_loop: t::MessageReceiver,
    mut network_error_recv: t::NetworkErrorReceiver,
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
    Box::pin(async move {
        let mut senders: Senders = HashMap::new();
        senders.insert(
            t::ProcessId::Name("filesystem".into()),
            ProcessSender::Runtime(send_to_fs),
        );
        senders.insert(
            t::ProcessId::Name("http_server".into()),
            ProcessSender::Runtime(send_to_http_server),
        );
        senders.insert(
            t::ProcessId::Name("http_client".into()),
            ProcessSender::Runtime(send_to_http_client),
        );
        senders.insert(
            t::ProcessId::Name("lfs".into()),
            ProcessSender::Runtime(send_to_lfs),
        );
        senders.insert(
            t::ProcessId::Name("net".into()),
            ProcessSender::Runtime(send_to_net.clone()),
        );

        // each running process is stored in this map
        let mut process_handles: ProcessHandles = HashMap::new();
        let mut is_debug: bool = false;

        // technical bootsequence.
        // will boot every wasm module inside /modules
        // currently have an exclude list to avoid broken moduels
        // modules started manually by users will get bootup automatically
        let exclude_list: Vec<t::ProcessId> = vec![
            t::ProcessId::Name("apps_home".into()),
            t::ProcessId::Name("explorer".into()),
            t::ProcessId::Name("http_bindings".into()),
            t::ProcessId::Name("http_proxy".into()),
            t::ProcessId::Name("process_manager".into()),
        ];

        for (process_id, wasm_bytes_handle) in process_map {
            if !exclude_list.contains(&process_id) {
                send_to_loop
                    .send(t::KernelMessage {
                        id: rand::random(),
                        source: t::Address {
                            node: our_name.clone(),
                            process: t::ProcessId::Name("kernel".into()),
                        },
                        target: t::Address {
                            node: our_name.clone(),
                            process: t::ProcessId::Name("kernel".into()),
                        },
                        rsvp: None,
                        message: t::Message::Request(t::Request {
                            inherit: false,
                            expects_response: false,
                            ipc: Some(
                                serde_json::to_string(&t::KernelCommand::StartProcess {
                                    name: match process_id {
                                        t::ProcessId::Name(name) => Some(name.into()),
                                        t::ProcessId::Id(_) => None,
                                    },
                                    wasm_bytes_handle,
                                    on_panic: t::OnPanic::None, // ADD to process map!
                                })
                                .unwrap(),
                            ),
                            metadata: None,
                        }),
                        payload: None,
                    })
                    .await
                    .unwrap();
            }
        }

        // main message loop
        loop {
            tokio::select! {
                // debug mode toggle: when on, this loop becomes a manual step-through
                debug = recv_debug_in_loop.recv() => {
                    if let Some(t::DebugCommand::Toggle) = debug {
                        is_debug = !is_debug;
                    }
                },
                ne = network_error_recv.recv() => {
                    let wrapped_network_error = ne.expect("fatal: event loop died");
                    let _ = send_to_terminal.send(
                        t::Printout {
                            verbosity: 1,
                            content: format!("event loop: got network error: {:?}", wrapped_network_error)
                        }
                    ).await;
                    // forward the error to the relevant process
                    match senders.get(&wrapped_network_error.source.process) {
                        Some(ProcessSender::Userspace(sender)) => {
                            // TODO: this failing should crash kernel
                            sender.send(Err(wrapped_network_error)).await.unwrap();
                        }
                        Some(ProcessSender::Runtime(_sender)) => {
                            // TODO should runtime modules get these? no
                            // this will change if a runtime process ever makes
                            // a message directed to not-our-node
                        }
                        None => {
                            send_to_terminal
                                .send(t::Printout {
                                    verbosity: 0,
                                    content: format!(
                                        "event loop: don't have {:?} amongst registered processes: {:?}",
                                        wrapped_network_error.source.process,
                                        senders.keys().collect::<Vec<_>>()
                                    )
                                })
                                .await
                                .unwrap();
                        }
                    }
                },
                kernel_message = recv_in_loop.recv() => {
                    let kernel_message = kernel_message.expect("fatal: event loop died");
                    while is_debug {
                        let debug = recv_debug_in_loop.recv().await.unwrap();
                        match debug {
                            t::DebugCommand::Toggle => is_debug = !is_debug,
                            t::DebugCommand::Step => break,
                        }
                    }
                    // display every single event when verbose
                    let _ = send_to_terminal.send(
                            t::Printout {
                                verbosity: 1,
                                content: format!("event loop: got message: {}", kernel_message)
                            }
                        ).await;
                    if our_name != kernel_message.target.node {
                        // unrecoverable if fails
                        send_to_net.send(kernel_message).await.expect("fatal: net module died");
                    } else {
                        if kernel_message.target.process == "kernel" {
                            // kernel only accepts messages from our own node
                            if our_name != kernel_message.source.node {
                                continue;
                            }
                            match kernel_message.message {
                                t::Message::Request(_) => {
                                    handle_kernel_request(
                                        our_name.clone(),
                                        kernel_message,
                                        send_to_loop.clone(),
                                        send_to_terminal.clone(),
                                        &mut senders,
                                        &mut process_handles,
                                    ).await;
                                }
                                t::Message::Response(_) => {
                                    handle_kernel_response(
                                        our_name.clone(),
                                        home_directory_path.clone(),
                                        kernel_message,
                                        send_to_loop.clone(),
                                        send_to_terminal.clone(),
                                        &mut senders,
                                        &mut process_handles,
                                        &engine,
                                    ).await;
                                }
                            }
                        } else {
                            // pass message to appropriate runtime module or process
                            match senders.get(&kernel_message.target.process) {
                                Some(ProcessSender::Userspace(sender)) => {
                                    // TODO: this failing should crash kernel
                                    sender.send(Ok(kernel_message)).await.unwrap();
                                }
                                Some(ProcessSender::Runtime(sender)) => {
                                    sender.send(kernel_message).await.unwrap();
                                }
                                None => {
                                    send_to_terminal
                                        .send(t::Printout {
                                            verbosity: 0,
                                            content: format!(
                                                "event loop: don't have {:?} amongst registered processes: {:?}",
                                                kernel_message.target.process,
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
    })
}

/// kernel entry point. creates event loop which contains all WASM processes
pub async fn kernel(
    our: t::Identity,
    home_directory_path: String,
    process_map: HashMap<t::ProcessId, u128>,
    send_to_loop: t::MessageSender,
    send_to_terminal: t::PrintSender,
    recv_in_loop: t::MessageReceiver,
    network_error_recv: t::NetworkErrorReceiver,
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
            home_directory_path,
            process_map,
            recv_in_loop,
            network_error_recv,
            recv_debug_in_loop,
            send_to_loop.clone(),
            send_to_wss,
            send_to_fs,
            send_to_lfs,
            send_to_http_server,
            send_to_http_client,
            send_to_terminal.clone(),
            engine,
        )
        .await,
    );
    let _ = event_loop_handle.await;
}
