use std::collections::{HashMap, HashSet};
use serde::{Serialize, Deserialize};
use bindings::print_to_terminal;
use bindings::component::microkernel_process::types::WitMessageType;
use bindings::component::microkernel_process::types::WitPayload;
use bindings::component::microkernel_process::types::WitProtomessageType;
use bindings::component::microkernel_process::types::WitRequestTypeWithTarget;

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
struct FileSystemResponseContext {
    process_name: String,
    wasm_bytes_uri: String,
}

fn send_stop_to_loop(
    our_name: String,
    process_name: String,
    is_expecting_response: bool,
) -> () {
    let kernel_stop_process_request = bindings::WitProtomessage {
        protomessage_type: WitProtomessageType::Request(
            WitRequestTypeWithTarget {
                is_expecting_response,
                target_ship: &our_name,
                target_app: "kernel",
            },
        ),
        payload: &WitPayload {
            json: Some(
                serde_json::to_string(
                    &KernelRequest::StopProcess(KernelStopProcess { process_name })
                ).unwrap()
            ),
            bytes: None,
        },
    };

    bindings::yield_results(vec![(kernel_stop_process_request, "")].as_slice());
}

impl bindings::MicrokernelProcess for Component {
    fn run_process(our_name: String, process_name: String) {
        print_to_terminal("process_manager: begin");

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
            let Some(s) = message.payload.json else {
                print_to_terminal(
                    format!(
                        "process manager: got payload with no json source, target: {:?}, {:?} {:?} {:?}",
                        message.wire.source_ship,
                        message.wire.source_app,
                        message.wire.target_ship,
                        message.wire.target_app,
                    ).as_str()
                );
                continue;
            };
            match message.message_type {
                WitMessageType::Request(_is_expecting_response) => {
                    let process_manager_command: ProcessManagerCommand =
                        serde_json::from_str(&s)
                        .expect("process manager: could not parse to command");
                    match process_manager_command {
                        ProcessManagerCommand::Start(start) => {
                            print_to_terminal("process manager: start");
                            if reserved_process_names.contains(&start.process_name) {
                                print_to_terminal(
                                    format!(
                                        "process manager: cannot add process {} with name amongst {:?}",
                                        &start.process_name,
                                        reserved_process_names.iter().collect::<Vec<_>>(),
                                    ).as_str()
                                );
                                continue;
                            }

                            let get_bytes_request = bindings::WitProtomessage {
                                protomessage_type: WitProtomessageType::Request(
                                    WitRequestTypeWithTarget {
                                        is_expecting_response: true,
                                        target_ship: &our_name,
                                        target_app: "filesystem",
                                    },
                                ),
                                payload: &WitPayload {
                                    json: Some(
                                        serde_json::to_string(
                                            &FileSystemRequest {
                                                uri_string: start.wasm_bytes_uri.clone(),
                                                action: FileSystemAction::Read,
                                            }
                                        ).unwrap()
                                    ),
                                    bytes: None,
                                },
                            };
                            let context = serde_json::to_string(&FileSystemResponseContext{
                                process_name: start.process_name,
                                wasm_bytes_uri: start.wasm_bytes_uri,
                            }).unwrap();
                            bindings::yield_results(vec![(get_bytes_request, context.as_str())].as_slice());
                        },
                        ProcessManagerCommand::Stop(stop) => {
                            print_to_terminal("process manager: stop");
                            let _ = metadatas
                                .remove(&stop.process_name)
                                .unwrap();

                            send_stop_to_loop(
                                our_name.clone(),
                                stop.process_name,
                                false,
                            );

                            println!("process manager: {:?}", metadatas.keys().collect::<Vec<_>>());
                        },
                        ProcessManagerCommand::Restart(restart) => {
                            print_to_terminal("process manager: restart");

                            send_stop_to_loop(
                                our_name.clone(),
                                restart.process_name,
                                true,
                            );
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
                            let context: FileSystemResponseContext =
                                serde_json::from_str(&context).unwrap();

                            let kernel_start_process_request = bindings::WitProtomessage {
                                protomessage_type: WitProtomessageType::Request(
                                    WitRequestTypeWithTarget {
                                        is_expecting_response: true,
                                        target_ship: &our_name,
                                        target_app: "kernel",
                                    },
                                ),
                                payload: &WitPayload {
                                    json: Some(
                                        serde_json::to_string(
                                            &KernelRequest::StartProcess(ProcessStart {
                                                process_name: context.process_name,
                                                wasm_bytes_uri: context.wasm_bytes_uri,
                                            })
                                        ).unwrap()
                                    ),
                                    bytes: Some(wasm_bytes),
                                },
                            };

                            bindings::yield_results(
                                vec![(kernel_start_process_request, "")].as_slice(),
                            );
                        },
                        (
                            our_name,
                            "kernel",
                            None,
                        ) => {
                            match serde_json::from_str(&s) {
                                Ok(KernelResponse::StartProcess(metadata)) => {
                                    metadatas.insert(
                                        metadata.process_name.clone(),
                                        metadata,
                                    );
                                    //  TODO: response?
                                    continue;
                                },
                                Ok(KernelResponse::StopProcess(stop)) => {
                                    let removed = metadatas
                                        .remove(&stop.process_name)
                                        .unwrap();

                                    let pm_start_request = bindings::WitProtomessage {
                                        protomessage_type: WitProtomessageType::Request(
                                            WitRequestTypeWithTarget {
                                                is_expecting_response: true,
                                                target_ship: &our_name,
                                                target_app: &process_name,
                                            },
                                        ),
                                        payload: &WitPayload {
                                            json: Some(
                                                serde_json::to_string(
                                                    &ProcessManagerCommand::Start(ProcessStart {
                                                        process_name: removed.process_name,
                                                        wasm_bytes_uri: removed.wasm_bytes_uri,
                                                    })
                                                ).unwrap()
                                            ),
                                            bytes: None,
                                        },
                                    };

                                    bindings::yield_results(vec![(pm_start_request, "")].as_slice());
                                    continue;
                                },
                                Err(e) => {
                                    print_to_terminal(
                                        format!(
                                            "{}: kernel response unexpected case; error: {} stack",
                                            process_name,
                                            e,
                                            ).as_str(),
                                    );
                                    continue;
                                },
                            }
                        },
                        _ => {
                            //  TODO: handle error or bail?
                            print_to_terminal(
                                "process_manager: response unexpected case; stack",
                            );
                            continue;
                        },
                    }
                },
            }
        }
    }
}

bindings::export!(Component);
