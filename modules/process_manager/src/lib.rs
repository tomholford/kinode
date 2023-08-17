cargo_component_bindings::generate!();

use std::collections::{HashMap, HashSet};
use serde::{Serialize, Deserialize};

use bindings::print_to_terminal;
use bindings::component::microkernel_process::types;

mod process_lib;

struct Component;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Payload {
    json: Option<serde_json::Value>,
    bytes: Option<Vec<u8>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestOnPanic {
    target: ProcessNode,
    payload: Payload,
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
    Start { process_name: String, wasm_bytes_uri: String, send_on_panic: SendOnPanic },
    Stop { process_name: String },
    Restart { process_name: String },
    ListRunningProcesses,
}
#[derive(Debug, Serialize, Deserialize)]
pub enum ProcessManagerResponse {
    ListRunningProcesses { processes: Vec<String> },
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

#[derive(Debug, Serialize, Deserialize)]
pub enum KernelRequest {
    StartProcess { process_name: String, wasm_bytes_uri: String, send_on_panic: SendOnPanic },
    StopProcess { process_name: String },
}
#[derive(Debug, Serialize, Deserialize)]
pub enum KernelResponse {
    StartProcess(ProcessMetadata),
    StopProcess { process_name: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessNode {
    pub node: String,
    pub process: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessMetadata {
    pub our: ProcessNode,
    pub wasm_bytes_uri: String,  // TODO: for use in restarting erroring process, ala midori
    send_on_panic: SendOnPanic,
}

type ProcessMetadatas = HashMap<String, ProcessMetadata>;

#[derive(Debug, Serialize, Deserialize)]
struct FileSystemReadContext {
    process_name: String,
    wasm_bytes_uri: String,
    send_on_panic: SendOnPanic,
}

fn send_stop_to_loop(
    our_name: String,
    process_name: String,
    is_expecting_response: bool,
) -> anyhow::Result<()> {
    process_lib::send_one_request(
        is_expecting_response,
        &our_name,
        "kernel",
        Some(KernelRequest::StopProcess { process_name }),
        None,
        None::<FileSystemReadContext>,
    )
}

fn handle_message(
    metadatas: &mut ProcessMetadatas,
    our_name: &str,
    reserved_process_names: &HashSet<String>,
) -> anyhow::Result<()> {
    let (message, context) = bindings::await_next_message()?;
    if our_name != message.source.node {
        return Err(anyhow::anyhow!("rejecting foreign Message from {:?}", message.source));
    }
    match message.content.message_type {
        types::WitMessageType::Request(_is_expecting_response) => {
            match process_lib::parse_message_json(message.content.payload.json)? {
                ProcessManagerCommand::Start { process_name, wasm_bytes_uri, send_on_panic } => {
                    print_to_terminal(1, "process manager: start");
                    if reserved_process_names.contains(&process_name) {
                        return Err(anyhow::anyhow!(
                            "cannot add process {} with name amongst {:?}",
                            &process_name,
                            reserved_process_names.iter().collect::<Vec<_>>(),
                        ))
                    }

                    process_lib::send_one_request(
                        true,
                        &our_name,
                        "filesystem",
                        Some(FileSystemRequest {
                            uri_string: wasm_bytes_uri.clone(),
                            action: FileSystemAction::Read,
                        }),
                        None,
                        Some(FileSystemReadContext {
                            process_name,
                            wasm_bytes_uri,
                            send_on_panic,
                        }),
                    )?;
                },
                ProcessManagerCommand::Stop { process_name } => {
                    print_to_terminal(1, "process manager: stop");
                    let _ = metadatas
                        .remove(&process_name)
                        .ok_or(anyhow::anyhow!("no process data found to remove"))?;

                    send_stop_to_loop(our_name.into(), process_name, false)?;

                    println!("process manager: {:?}\r", metadatas.keys().collect::<Vec<_>>());
                },
                ProcessManagerCommand::Restart { process_name } => {
                    print_to_terminal(1, "process manager: restart");

                    send_stop_to_loop(our_name.into(), process_name, true)?;
                },
                ProcessManagerCommand::ListRunningProcesses => {
                    process_lib::send_response(
                        Some(ProcessManagerResponse::ListRunningProcesses {
                            processes: metadatas.iter()
                                .map(|(key, _value)| key.clone())
                                .collect()
                        }),
                        None,
                        None::<FileSystemReadContext>,
                    )?;
                },
            }
        },
        types::WitMessageType::Response => {
            match (
                message.source.node,
                message.source.process.as_str(),
                message.content.payload.bytes,
            ) {
                (
                    our_name,
                    "filesystem",
                    Some(wasm_bytes),
                ) => {
                    let context: FileSystemReadContext = serde_json::from_str(&context)?;

                    process_lib::send_one_request(
                        true,
                        &our_name,
                        "kernel",
                        Some(KernelRequest::StartProcess {
                            process_name: context.process_name,
                            wasm_bytes_uri: context.wasm_bytes_uri,
                            send_on_panic: context.send_on_panic,
                        }),
                        Some(wasm_bytes),
                        None::<FileSystemReadContext>,
                    )?;
                },
                (
                    our_name,
                    "kernel",
                    None,
                ) => {
                    match process_lib::parse_message_json(message.content.payload.json)? {
                        KernelResponse::StartProcess(metadata) => {
                            metadatas.insert(
                                metadata.our.process.clone(),
                                metadata.clone(),
                            );
                            process_lib::send_response(
                                Some(KernelResponse::StartProcess(metadata)),
                                None,
                                None::<FileSystemReadContext>,
                            )?;
                        },
                        KernelResponse::StopProcess { process_name } => {
                            let removed = metadatas
                                .remove(&process_name)
                                .ok_or(anyhow::anyhow!("no process data found to remove"))?;

                            process_lib::send_one_request(
                                true,
                                &our_name,
                                &process_name,
                                Some(ProcessManagerCommand::Start {
                                    process_name: removed.our.process,
                                    wasm_bytes_uri: removed.wasm_bytes_uri,
                                    send_on_panic: removed.send_on_panic,
                                }),
                                None,
                                None::<FileSystemReadContext>,
                            )?;
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

impl bindings::MicrokernelProcess for Component {
    fn run_process(our_name: String, process_name: String) {
        print_to_terminal(1, "process_manager: begin");

        let reserved_process_names = vec![
            "filesystem".to_string(),
            our_name.clone(),
        ];
        let reserved_process_names: HashSet<String> = reserved_process_names
            .into_iter()
            .collect();
        let mut metadatas: ProcessMetadatas = HashMap::new();
        loop {
            match handle_message(
                &mut metadatas,
                &our_name,
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
