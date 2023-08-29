cargo_component_bindings::generate!();

use std::collections::{HashMap, HashSet};
use serde::{Serialize, Deserialize};

use bindings::{MicrokernelProcess, get_insecure_uniform_u64, print_to_terminal, receive, send};
use bindings::component::microkernel_process::types;

mod process_lib;

struct Component;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TransitPayloadBytes {
    None,
    Some(Vec<u8>),
    Circumvent(Vec<u8>),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestOnPanic {
    pub target: ProcessReference,
    pub json: Option<String>,
    pub bytes: TransitPayloadBytes,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SendOnPanic {
    None,
    Restart,
    Requests(Vec<RequestOnPanic>),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProcessManagerCommand {
    Initialize { jwt_secret_bytes: Option<Vec<u8>> },
    Start { name: Option<String>, wasm_bytes_uri: String, send_on_panic: SendOnPanic },
    Stop { id: u64 },
    Restart { id: u64 },
    ListRegisteredProcesses,
    PersistState,
    RebootStart { id: u64, name: Option<String>, wasm_bytes_uri: String, send_on_panic: SendOnPanic },  //  TODO: remove
}
#[derive(Debug, Serialize, Deserialize)]
pub enum ProcessManagerResponse {
    Initialize,
    Start { id: u64, name: Option<String> },
    ListRegisteredProcesses { processes: Vec<String> },
    PersistState([u8; 32]),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileSystemRequest {
    pub uri_string: String,
    pub action: FileSystemAction,
}
#[derive(Debug, Serialize, Deserialize)]
pub enum FileSystemAction {
    Read,
    Write,
    OpenRead,
    OpenWrite,
    Append,
    ReadChunkFromOpen(u64),
    SeekWithinOpen(FileSystemSeekFrom),
}
//  copy of std::io::SeekFrom with Serialize/Deserialize
#[derive(Debug, Serialize, Deserialize)]
pub enum FileSystemSeekFrom {
    Start(u64),
    End(i64),
    Current(i64),
}

#[derive(Serialize, Deserialize, Debug)]
pub enum FsAction {
    Write,
    Append(Option<[u8; 32]>),
    Read([u8; 32]),
    ReadChunk(ReadChunkRequest),
    PmWrite,                     //  specific case for process manager persistance.
    Delete([u8; 32]),
    Length([u8; 32]),
}

#[derive(Serialize, Deserialize, Debug)]
pub enum FsResponse {
    //  bytes are in payload_bytes
    Read([u8; 32]),
    ReadChunk([u8; 32]),
    Write([u8; 32]),
    Append([u8; 32]),
    Delete([u8; 32]),
    Length(u64),
    //  use FileSystemError
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ReadChunkRequest {
    file_hash: [u8; 32],
    start: u64,
    length: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum KernelRequest {
    StartProcess {
        id: u64,
        name: Option<String>,
        wasm_bytes_uri: String,
        send_on_panic: SendOnPanic,
    },
    StopProcess { id: u64 },
    RegisterProcess { id: u64, name: String },
    UnregisterProcess { id: u64 },
}
#[derive(Debug, Serialize, Deserialize)]
pub enum KernelResponse {
    StartProcess(ProcessMetadata),
    StopProcess { id: u64 },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessReference {
    pub node: String,
    pub identifier: ProcessIdentifier,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessAddress {
    pub node: String,
    pub id: u64,
    pub name: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProcessIdentifier {
    Id(u64),
    Name(String),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessMetadata {
    pub our: ProcessAddress,
    pub wasm_bytes_uri: String,  // TODO: for use in restarting erroring process, ala midori
    send_on_panic: SendOnPanic,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Process {
    metadata: ProcessMetadata,
    persisted_state_handle: Option<[u8; 32]>,
}

type Names = HashMap<String, u64>;
type Processes = HashMap<u64, Process>;

#[derive(Debug, Serialize, Deserialize)]
enum Context {
    FileSystemRead {
        id: u64,
        name: Option<String>,
        wasm_bytes_uri: String,
        send_on_panic: SendOnPanic,
    },
    Persist {
        identifier: ProcessIdentifier,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SequentializeRequest {
    QueueMessage {
        target_node: Option<String>,
        target_process: ProcessIdentifier,
        json: Option<String>,
    },
    RunQueue,
}

fn send_stop_to_loop(
    our_name: String,
    process_id: u64,
    is_expecting_response: bool,
) -> anyhow::Result<()> {
    process_lib::send_one_request(
        is_expecting_response,
        &our_name,
        types::ProcessIdentifier::Name("kernel".into()),
        Some(KernelRequest::StopProcess { id: process_id }),
        types::OutboundPayloadBytes::None,
        None::<Context>,
    )
}

fn persist_pm_state(
    our_name: &str,
    processes: &Processes,
) -> anyhow::Result<Result<types::InboundMessage, types::UqbarError>> {
    print_to_terminal(1, "process_manager: persist pm state");
    process_lib::send_and_await_receive(
        our_name.into(),
        types::ProcessIdentifier::Name("lfs".into()),
        Some(FsAction::PmWrite),
        types::OutboundPayloadBytes::Some(bincode::serialize(processes)?),
    )
}

fn derive_names(processes: &Processes) -> Names {
    processes
        .iter()
        .filter_map(|(key, process)| {
            match process.metadata.our.name {
                None => None,
                Some(ref name) => Some((name.clone(), *key)),
            }
        })
        .collect()
}

fn de_wit_process_identifier(wit: &types::ProcessIdentifier) -> ProcessIdentifier {
    match wit {
        types::ProcessIdentifier::Id(id) => ProcessIdentifier::Id(id.clone()),
        types::ProcessIdentifier::Name(name) => ProcessIdentifier::Name(name.clone()),
    }
}

fn remove_process(
    id: u64,
    processes: &mut Processes,
    names: &mut Names,
) -> anyhow::Result<Process> {
    let removed = processes
        .remove(&id)
        .ok_or(anyhow::anyhow!("no process data found to remove"))?;
    match removed.metadata.our.name {
        None => {},
        Some(ref name) => {
            let _ = names.remove(name);
        },
    }

    Ok(removed)
}

fn begin_start_process(
    id: u64,
    name: Option<String>,
    wasm_bytes_uri: String,
    send_on_panic: SendOnPanic,
    our_name: &str,
    reserved_process_names: &HashSet<String>,
    processes: &mut Processes,
    names: &mut Names,
) -> anyhow::Result<()> {
    print_to_terminal(1, "process manager: start");
    match name {
        None => {},
        Some(ref name) => {
            if reserved_process_names.contains(name) {
                return Err(anyhow::anyhow!(
                    "cannot add process {} with name amongst {:?}",
                    name,
                    reserved_process_names.iter().collect::<Vec<_>>(),
                ))
            }
        },
    }

    //  store in memory until get KernelResponse::StartProcess
    processes.insert(
        id.clone(),
        Process {
            metadata: ProcessMetadata {
                our: ProcessAddress {
                    node: our_name.into(),
                    id: id.clone(),
                    name: name.clone(),
                },
                wasm_bytes_uri: wasm_bytes_uri.clone(),
                send_on_panic: send_on_panic.clone(),
            },
            persisted_state_handle: None,
        },
    );
    if let Some(ref n) = name {
        names.insert(n.clone(), id.clone());
    }

    process_lib::send_one_request(
        true,
        &our_name,
        types::ProcessIdentifier::Name("filesystem".into()),
        Some(FileSystemRequest {
            uri_string: wasm_bytes_uri.clone(),
            action: FileSystemAction::Read,
        }),
        types::OutboundPayloadBytes::None,
        Some(Context::FileSystemRead {
            id,
            name,
            wasm_bytes_uri,
            send_on_panic,
        }),
    )?;

    Ok(())
}

fn queue_reboot_messages(
    our_name: &str,
    our_process_name: &str,
    sequentialize_process_name: &str,
    process_id: &u64,
    process: &Process,
) -> anyhow::Result<Vec<types::JoinedRequests>> {
    let wasm_bytes_uri = process.metadata.wasm_bytes_uri.clone();
    let send_on_panic = process.metadata.send_on_panic.clone();

    //  TODO: to kill processes without any persisted state on reboot,
    //        move this into the `if let Some(handle) = process.persisted_state_handle {}`
    //        case
    let start_request = process_lib::make_request(
        false,
        our_name.into(),
        types::ProcessIdentifier::Name(sequentialize_process_name.into()),
        Some(SequentializeRequest::QueueMessage {
            target_node: Some(our_name.into()),
            target_process: ProcessIdentifier::Name(our_process_name.into()),
            json: Some(serde_json::to_string(
                &ProcessManagerCommand::RebootStart {
                    id: process.metadata.our.id.clone(),
                    name: process.metadata.our.name.clone(),
                    wasm_bytes_uri,
                    send_on_panic,
                }
            )?),
        }),
        types::OutboundPayloadBytes::None,
        None::<Context>,
    )?;

    if let Some(handle) = process.persisted_state_handle {
        let response = process_lib::send_and_await_receive(
            our_name.into(),
            types::ProcessIdentifier::Name("lfs".into()),
            Some(FsAction::Read(handle)),
            types::OutboundPayloadBytes::None,
        )?;

        let response_payload = match response {
            Ok(types::InboundMessage::Request(request)) => request.payload,
            Ok(types::InboundMessage::Response(payload)) => payload,
            Err(e) => panic!("process_manager: got error queueing reboot messages: {:?}", e),
        };
        let outbound_bytes = process_lib::make_outbound_bytes_from_noncircumvented_inbound(
            response_payload.bytes,
        )?;

        match process_lib::parse_message_json(response_payload.json)? {
            FsResponse::Read(_) => {
                let initialize_request = process_lib::make_request(
                    false,
                    our_name.into(),
                    types::ProcessIdentifier::Name(sequentialize_process_name.into()),
                    Some(SequentializeRequest::QueueMessage {
                        target_node: Some(our_name.into()),
                        target_process: ProcessIdentifier::Id(process_id.clone()),
                        json: Some(
                            serde_json::to_string(&serde_json::json!({"Initialize": null}))?
                        ),
                    }),
                    outbound_bytes,
                    None::<Context>,
                )?;
                Ok(vec![start_request, initialize_request])
            },
            _ => {
                Err(
                    anyhow::anyhow!("got unexpected Fs Response while reading persisted state")
                )
            },
        }
    } else {
        Ok(vec![start_request])
    }
}

fn handle_message (
    processes: &mut Processes,
    names: &mut Names,
    our_name: &str,
    process_name: &str,
    reserved_process_names: &HashSet<String>,
) -> anyhow::Result<()> {
    print_to_terminal(1, "pm: waiting on message");
    let (message, context) = receive()?;
    match message {
        types::InboundMessage::Request(types::InboundRequest {
            is_expecting_response: _,
            payload: types::InboundPayload {
                source,
                json,
                bytes,
            },
        }) => {
            if our_name != source.node {
                return Err(anyhow::anyhow!(
                    "rejecting foreign Message from {:?}",
                    source,
                ));
            }
            match process_lib::parse_message_json(json)? {
                ProcessManagerCommand::Initialize{ jwt_secret_bytes } => {
                    print_to_terminal(0, "process manager: init");

                    match bytes {
                        types::InboundPayloadBytes::Circumvented => {
                            panic!("process manager: got invalid Circumvented for Initialize");
                        },
                        types::InboundPayloadBytes::None => {
                            //  starting from scratch -> set up persisted memory
                            let _response = persist_pm_state(our_name, processes);
                            process_lib::send_response(
                                Some(ProcessManagerResponse::Initialize),
                                types::OutboundPayloadBytes::None,
                                None::<Context>,
                            )?;
                            print_to_terminal(0, "process manager: init init boot done");
                        },
                        types::InboundPayloadBytes::Some(bytes) => {
                            //  rebooting -> load bytes in to memory & spin up processes
                            let Some(jwt_secret_bytes) = jwt_secret_bytes else {
                                return Err(anyhow::anyhow!("reboot requires jwt input"));
                            };
                            *processes = bincode::deserialize(&bytes[..])?;
                            *names = derive_names(processes);

                            let mut requests = vec![];

                            let name = "http_bindings";
                            let id = names
                                .get(name)
                                .ok_or(anyhow::anyhow!(
                                    "must have registered http_bindings to reboot"
                                ))?;
                            match processes.remove(id) {
                                None => {
                                    return Err(anyhow::anyhow!(
                                        "must have registered http_bindings to reboot"
                                    ));
                                },
                                Some(process) => {
                                    requests.append(&mut queue_reboot_messages(
                                        our_name,
                                        process_name,
                                        "sequentialize",
                                        id,
                                        &process,
                                    )?);
                                },
                            }

                            for (id, process) in processes {
                                requests.append(&mut queue_reboot_messages(
                                    our_name,
                                    process_name,
                                    "sequentialize",
                                    id,
                                    process,
                                )?);
                            }

                            requests.push(process_lib::make_request(
                                false,
                                our_name.into(),
                                types::ProcessIdentifier::Name("sequentialize".into()),
                                Some(SequentializeRequest::QueueMessage {
                                    target_node: Some(our_name.into()),
                                    target_process: ProcessIdentifier::Name(
                                        "http_bindings".into()
                                    ),
                                    json: Some(serde_json::to_string(
                                        &serde_json::json!({"action": "set-jwt-secret"})
                                    )?),
                                }),
                                types::OutboundPayloadBytes::Some(jwt_secret_bytes),
                                None::<Context>,
                            )?);

                            requests.push(process_lib::make_request(
                                false,
                                our_name.into(),
                                types::ProcessIdentifier::Name("sequentialize".into()),
                                Some(SequentializeRequest::RunQueue),
                                types::OutboundPayloadBytes::None,
                                None::<Context>,
                            )?);

                            print_to_terminal(0, &format!("len requests: {}", requests.len()));

                            send(Ok(&types::OutboundMessage::Requests(requests)));

                            print_to_terminal(0, "process manager: init reboot done");
                        },
                    }
                },
                ProcessManagerCommand::Start { name, wasm_bytes_uri, send_on_panic } => {
                    let id = get_insecure_uniform_u64();
                    begin_start_process(
                        id,
                        name,
                        wasm_bytes_uri,
                        send_on_panic,
                        our_name,
                        &reserved_process_names,
                        processes,
                        names,
                    )?;
                },
                ProcessManagerCommand::Stop { id } => {
                    print_to_terminal(1, "process manager: stop");
                    let _ = processes
                        .remove(&id)
                        .ok_or(anyhow::anyhow!("no process data found to remove"))?;

                    let _response = persist_pm_state(our_name, processes);
                    send_stop_to_loop(our_name.into(), id, false)?;

                    println!("process manager: {:?}\r", processes.keys().collect::<Vec<_>>());
                },
                ProcessManagerCommand::Restart { id } => {
                    print_to_terminal(1, "process manager: restart");

                    send_stop_to_loop(our_name.into(), id, true)?;
                },
                ProcessManagerCommand::ListRegisteredProcesses => {
                    process_lib::send_response(
                        Some(ProcessManagerResponse::ListRegisteredProcesses {
                            processes: names.iter()
                                .map(|(key, _value)| key.clone())
                                .collect()
                        }),
                        types::OutboundPayloadBytes::None,
                        None::<Context>,
                    )?;
                },
                ProcessManagerCommand::PersistState => {
                    match source.identifier {
                        types::ProcessIdentifier::Id(_) => {},
                        types::ProcessIdentifier::Name(ref name) => {
                            if !names.contains_key(name) {
                                return Err(anyhow::anyhow!(
                                    "cannot PersistState: '{}' not registered",
                                    name,
                                ));
                            }
                        },
                    }

                    let types::InboundPayloadBytes::Circumvented = bytes else {
                        return Err(anyhow::anyhow!(
                            "must use OutboundPayloadBytes::Circumvent to persist state",
                        ));
                    };

                    process_lib::send_one_request(
                        true,
                        our_name,
                        types::ProcessIdentifier::Name("lfs".into()),
                        Some(FsAction::Write),
                        types::OutboundPayloadBytes::AttachCircumvented,
                        Some(Context::Persist {
                            identifier: de_wit_process_identifier(&source.identifier)
                        }),
                    )?;
                },
                ProcessManagerCommand::RebootStart { id, name, wasm_bytes_uri, send_on_panic } => {
                    begin_start_process(
                        id,
                        name,
                        wasm_bytes_uri,
                        send_on_panic,
                        our_name,
                        &reserved_process_names,
                        processes,
                        names,
                    )?;
                },
            }
        },
        types::InboundMessage::Response(types::InboundPayload {
            source,
            json,
            bytes,
        }) => {
            if our_name != source.node {
                return Err(anyhow::anyhow!(
                    "rejecting foreign Message from {:?}",
                    source,
                ));
            }
            let types::ProcessIdentifier::Name(process) = source.identifier else {
                return Err(anyhow::anyhow!("Response case must have name identifier"))
            };
            match (
                source.node,
                process.as_str(),
                bytes,
            ) {
                (
                    our_name,
                    "filesystem",
                    types::InboundPayloadBytes::Some(wasm_bytes),
                ) => {
                    print_to_terminal(1, "process manager: got filesystem Response");
                    let Context::FileSystemRead {
                        id,
                        name,
                        wasm_bytes_uri,
                        send_on_panic,
                    } = serde_json::from_str(&context)? else {
                        return Err(
                            anyhow::anyhow!(
                                "got filesystem Response with incorrect context. Response: {:?}. Context: {}",
                                json,
                                context,
                            )
                        );
                    };

                    process_lib::send_one_request(
                        true,
                        &our_name,
                        types::ProcessIdentifier::Name("kernel".into()),
                        Some(KernelRequest::StartProcess {
                            id,
                            name,
                            wasm_bytes_uri,
                            send_on_panic,
                        }),
                        types::OutboundPayloadBytes::Some(wasm_bytes),
                        None::<Context>,
                    )?;
                },
                (
                    our_name,
                    "kernel",
                    types::InboundPayloadBytes::None,
                ) => {
                    print_to_terminal(1, "process manager: got kernel Response");
                    match process_lib::parse_message_json(json)? {
                        KernelResponse::StartProcess(metadata) => {
                            if processes.contains_key(&metadata.our.id) {
                                let _response = persist_pm_state(&our_name, processes);
                            }
                            process_lib::send_response(
                                Some(ProcessManagerResponse::Start {
                                    id: metadata.our.id,
                                    name: metadata.our.name,
                                }),
                                types::OutboundPayloadBytes::None,
                                None::<Context>,
                            )?;
                        },
                        KernelResponse::StopProcess { id } => {
                            let removed = remove_process(id, processes, names)?;
                            // let removed = processes
                            //     .remove(&id)
                            //     .ok_or(anyhow::anyhow!("no process data found to remove"))?;
                            let _response = persist_pm_state(&our_name, processes);
                            process_lib::send_one_request(
                                true,
                                &our_name,
                                types::ProcessIdentifier::Name(process_name.into()),
                                Some(ProcessManagerCommand::Start {
                                    name: removed.metadata.our.name,
                                    wasm_bytes_uri: removed.metadata.wasm_bytes_uri,
                                    send_on_panic: removed.metadata.send_on_panic,
                                }),
                                types::OutboundPayloadBytes::None,
                                None::<Context>,
                            )?;
                        },
                    }
                },
                (
                    our_name,
                    "lfs",
                    types::InboundPayloadBytes::None,
                ) => {
                    match process_lib::parse_message_json(json)? {
                        FsResponse::Write(handle) => {
                            let Context::Persist { identifier } = serde_json::from_str(&context)? else {
                                return Err(
                                    anyhow::anyhow!(
                                        "got lfs Response with incorrect context. Context: {}",
                                        context,
                                    )
                                );
                            };
                            let id = match identifier {
                                ProcessIdentifier::Id(ref id) => id,
                                ProcessIdentifier::Name(name) => {
                                    names
                                        .get(&name)
                                        .ok_or(anyhow::anyhow!(
                                            "did not find Persist context name"
                                        ))?
                                },
                            };
                            let process = processes.get_mut(id)
                                .ok_or(anyhow::anyhow!(
                                    "did not find process corresponding to lfs Write"
                                ))?;
                            process.persisted_state_handle = Some(handle);

                            let _response = persist_pm_state(&our_name, processes);

                            process_lib::send_response(
                                Some(ProcessManagerResponse::PersistState(handle)),
                                types::OutboundPayloadBytes::None,
                                None::<Context>,
                            )?;
                        },
                        _ => {
                            return Err(anyhow::anyhow!("unexpected LFS Response case"))
                        },
                    }
                },
                _ => {
                    //  TODO: handle error or bail?
                    return Err(anyhow::anyhow!("unexpected Response case"))
                },
            };
        },
    }

    Ok(())
}

impl MicrokernelProcess for Component {
    fn run_process(our: types::ProcessAddress) {
    // fn run_process(our_name: String, process_name: String) {
        print_to_terminal(1, "process_manager: begin");

        let Some(process_name) = our.name else {
            print_to_terminal(0, "process_manager: require our.name set");
            panic!();
        };

        let reserved_process_names = vec![
            "filesystem".to_string(),
            process_name.clone(),
        ];
        let reserved_process_names: HashSet<String> = reserved_process_names
            .into_iter()
            .collect();
        let mut processes: Processes = HashMap::new();
        let mut names: Names = HashMap::new();
        loop {
            match handle_message(
                &mut processes,
                &mut names,
                &our.node,
                &process_name,
                &reserved_process_names
            ) {
                Ok(()) => {},
                Err(e) => {
                    print_to_terminal(0, format!(
                        "{}: error: {:?}",
                        process_name,
                        e,
                    ).as_str());
                },
            };
        }
    }
}
