use std::collections::{HashMap, HashSet};
use serde::{Serialize, Deserialize};
use bindings::print_to_terminal;
use bindings::component::microkernel_process::types::WitMessage;
use bindings::component::microkernel_process::types::WitMessageType;

mod process_lib;

struct Component;

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

type ProcessMetadatas = HashMap<String, ProcessMetadata>;

#[derive(Debug, Serialize, Deserialize)]
struct FileSystemReadContext {
    process_name: String,
    wasm_bytes_uri: String,
}

fn send_stop_to_loop(
    our_name: String,
    process_name: String,
    is_expecting_response: bool,
) -> anyhow::Result<()> {
    process_lib::yield_one_request(
        is_expecting_response,
        &our_name,
        "kernel",
        Some(KernelRequest::StopProcess(KernelStopProcess { process_name })),
        None,
        None::<FileSystemReadContext>,
    )
}

fn handle_message(
    message: WitMessage,
    context: String,
    metadatas: &mut ProcessMetadatas,
    our_name: &str,
    process_name: &str,
    reserved_process_names: &HashSet<String>,
) -> anyhow::Result<()> {
    match message.message_type {
        WitMessageType::Request(_is_expecting_response) => {
            match process_lib::parse_message_json(message.payload.json)? {
                ProcessManagerCommand::Start(start) => {
                    print_to_terminal(1, "process manager: start");
                    if reserved_process_names.contains(&start.process_name) {
                        return Err(anyhow::anyhow!(
                            "cannot add process {} with name amongst {:?}",
                            &start.process_name,
                            reserved_process_names.iter().collect::<Vec<_>>(),
                        ))
                    }

                    process_lib::yield_one_request(
                        true,
                        our_name,
                        "filesystem",
                        Some(FileSystemRequest {
                            uri_string: start.wasm_bytes_uri.clone(),
                            action: FileSystemAction::Read,
                        }),
                        None,
                        Some(FileSystemReadContext {
                            process_name: start.process_name,
                            wasm_bytes_uri: start.wasm_bytes_uri,
                        }),
                    )?;
                },
                ProcessManagerCommand::Stop(stop) => {
                    print_to_terminal(1, "process manager: stop");
                    let _ = metadatas
                        .remove(&stop.process_name)
                        .ok_or(anyhow::anyhow!("no process data found to remove"))?;

                    send_stop_to_loop(
                        our_name.into(),
                        stop.process_name,
                        false,
                    )?;

                    println!("process manager: {:?}\r", metadatas.keys().collect::<Vec<_>>());
                },
                ProcessManagerCommand::Restart(restart) => {
                    print_to_terminal(1, "process manager: restart");

                    send_stop_to_loop(
                        our_name.into(),
                        restart.process_name,
                        true,
                    )?;
                },
            }
        },
        WitMessageType::Response => {
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
                    let context: FileSystemReadContext = serde_json::from_str(&context)?;

                    process_lib::yield_one_request(
                        true,
                        &our_name,
                        "kernel",
                        Some(KernelRequest::StartProcess(ProcessStart {
                            process_name: context.process_name,
                            wasm_bytes_uri: context.wasm_bytes_uri,
                        })),
                        Some(wasm_bytes),
                        None::<FileSystemReadContext>,
                    )?;
                },
                (
                    our_name,
                    "kernel",
                    None,
                ) => {
                    match process_lib::parse_message_json(message.payload.json)? {
                        KernelResponse::StartProcess(metadata) => {
                            metadatas.insert(
                                metadata.process_name.clone(),
                                metadata,
                            );
                            //  TODO: response?
                        },
                        KernelResponse::StopProcess(stop) => {
                            let removed = metadatas
                                .remove(&stop.process_name)
                                .ok_or(anyhow::anyhow!("no process data found to remove"))?;

                            process_lib::yield_one_request(
                                true,
                                &our_name,
                                process_name,
                                Some(ProcessManagerCommand::Start(ProcessStart {
                                    process_name: removed.process_name,
                                    wasm_bytes_uri: removed.wasm_bytes_uri,
                                })),
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
            "terminal".to_string(),
        ];
        let reserved_process_names: HashSet<String> = reserved_process_names
            .into_iter()
            .collect();
        let mut metadatas: ProcessMetadatas = HashMap::new();
        loop {
            let (message, context) = bindings::await_next_message();
            //  TODO: validate source/target?
            match handle_message(
                message,
                context,
                &mut metadatas,
                &our_name,
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

bindings::export!(Component);
