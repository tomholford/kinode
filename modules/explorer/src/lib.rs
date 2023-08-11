cargo_component_bindings::generate!();

use anyhow::anyhow;
use std::collections::HashMap;
use serde::{Serialize, Deserialize};
use thiserror::Error;
use bindings::print_to_terminal;
use bindings::component::microkernel_process::types::WitPayload;
use bindings::component::microkernel_process::types::WitProcessNode;
use bindings::component::microkernel_process::types::WitProtomessageType;
use bindings::component::microkernel_process::types::WitRequestTypeWithTarget;
use bindings::component::microkernel_process::types::WitUqbarError;

struct Component;

type FileHash = [u8; 32];
type HierarchicalFS = HashMap<String, FileHash>; // TODO this should be a trie

const UPLOAD_PAGE: &str = include_str!("upload.html");

fn div_round_up(numerator: u64, denominator: u64) -> u64 {
    (numerator + denominator - 1) / denominator
}

fn handle_next_message(
    file_names: &mut HierarchicalFS,
    our_name: &str,
    process_name: &str,
) -> anyhow::Result<()> {
    let (message, context) = bindings::await_next_message()?;
    let Some(ref payload_json_string) = message.content.payload.json else {
        return Err(anyhow!("require non-empty json payload"));
    };

    print_to_terminal(
        0,
        format!("{}: got json {}", &process_name, payload_json_string).as_str()
    );

    let message_from_loop: serde_json::Value = serde_json::from_str(&payload_json_string).unwrap();
    if message_from_loop["method"] == "GET" && message_from_loop["raw_path"] == "/apps/explorer/upload" {
        bindings::yield_results(Ok(vec![(
            bindings::WitProtomessage {
                protomessage_type: WitProtomessageType::Response,
                payload: WitPayload {
                    json: Some(serde_json::json!({
                        "action": "response",
                        "status": 200,
                        "headers": {
                            "Content-Type": "text/html",
                        },
                    }).to_string()),
                    bytes: Some(UPLOAD_PAGE.replace("${our}", &our_name).as_bytes().to_vec())
                }
            },
            "".into(),
        )].as_slice()));
        return Ok(());
    } else if message_from_loop["method"] == "POST" && message_from_loop["raw_path"] == "/apps/explorer/upload" {
        bindings::print_to_terminal(0, "got upload request");
        let body_bytes = message.content.payload.bytes.unwrap_or(vec![]); // TODO no unwrap
        bindings::print_to_terminal(0, format!("len {:?}", body_bytes.len()).as_str());
        // TODO put into FS
        return Ok(());
    } else {
        return Err(anyhow!("unrecognized request"));
    }
}

impl bindings::MicrokernelProcess for Component {
    fn run_process(our_name: String, process_name: String) {
        print_to_terminal(1, "file_transfer: begin");
        // HTTP bindings
        bindings::yield_results(Ok(
            vec![(
                bindings::WitProtomessage {
                    protomessage_type: WitProtomessageType::Request(
                        WitRequestTypeWithTarget {
                            is_expecting_response: false,
                            target: WitProcessNode {
                                node: our_name.clone(),
                                process: "http_bindings".into(),
                            },
                        }
                    ),
                    payload: WitPayload {
                        json: Some(serde_json::json!({
                            "action": "bind-app",
                            "path": "/apps/explorer/upload",
                            "app": process_name
                        }).to_string()),
                        bytes: None
                    }
                },
                "".into(),
            ), (
                bindings::WitProtomessage {
                    protomessage_type: WitProtomessageType::Request(
                        WitRequestTypeWithTarget {
                            is_expecting_response: false,
                            target: WitProcessNode {
                                node: our_name.clone(),
                                process: "http_bindings".into(),
                            },
                        }
                    ),
                    payload: WitPayload {
                        json: Some(serde_json::json!({
                            "action": "bind-app",
                            "path": "/apps/explorer/file/:path",
                            "app": process_name
                        }).to_string()),
                        bytes: None
                    }
                },
                "".into(),
            )].as_slice()
        ));

        let mut file_names: HierarchicalFS = HashMap::new();

        loop {
            match handle_next_message(
                &mut file_names,
                &our_name,
                &process_name,
            ) {
                Ok(_) => {},
                Err(e) => {
                    match e.downcast_ref::<WitUqbarError>() {
                        None => print_to_terminal(0, format!("{}: error: {:?}", process_name, e).as_str()),
                        Some(uqbar_error) => {
                            // TODO handle afs errors here
                            print_to_terminal(
                                0,
                                format!("{}: error: {:?}", process_name, e).as_str()
                            )
                        },
                    }
                },
            }
        }
    }
}
