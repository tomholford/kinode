use crate::types::*;
use tokio::fs;
use serde::{Serialize, Deserialize};

// TODO these should be in types.rs and shared across libraries
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
struct ProcessStart {
    process_name: String,
    wasm_bytes_uri: String,
}
//
// DONE

pub async fn pill(our: Identity) -> Vec<WrappedMessage> {
    // always start process manager on boot
    let process_manager_wasm_bytes = fs::read("./src/pill/process_manager.wasm").await.unwrap();
    let terminal_wasm_bytes        = fs::read("./src/pill/terminal.wasm").await.unwrap();
    let http_bindings_bytes        = fs::read("./src/pill/http_bindings.wasm").await.unwrap();
    let apps_home_bytes            = fs::read("./src/pill/apps_home.wasm").await.unwrap();
    let ft_bytes                   = fs::read("./src/pill/file_transfer.wasm").await.unwrap();

    vec![
        // start process manager
        WrappedMessage {
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
                                wasm_bytes_uri: "process_manager.wasm".to_string(),
                            }
                        )
                    ).unwrap()),
                    bytes: Some(process_manager_wasm_bytes),
                },
            },
        },
        // start terminal
        WrappedMessage {
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
        },
        // start http_bindings
        WrappedMessage {
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
        },
        // start apps_home
        WrappedMessage {
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
        },
        // start file transfer
        WrappedMessage {
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
        }
    ]
}