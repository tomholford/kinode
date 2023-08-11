mod types;
use crate::types::*;
use tokio::fs;

pub async fn pill() -> Vec<BinSerializableWrappedMessage> {
    //  add new processes_to_start here
    let mut processes_to_start = vec![
        "process_manager",
        "terminal",
        "http_bindings",
        "apps_home",
        "http_proxy",
        "file_transfer",
    ];

    let mut boot_sequence: Vec<BinSerializableWrappedMessage> = Vec::new();

    //  copy wasm bytes into home dir
    for process in &processes_to_start {
        let uri_string = format!("fs://{}.wasm", process);
        let process_wasm_path =
            format!("../modules/{process}/target/wasm32-unknown-unknown/release/{process}.wasm");
        let process_wasm_bytes = fs::read(&process_wasm_path).await.expect(&process_wasm_path);
        let save_process_message = BinSerializableWrappedMessage {
            id: rand::random(),
            //  target node assigned by runtime to "our"
            target_process: "filesystem".into(),
            //  rsvp assigned by runtime (as None)
            message: BinSerializableMessage {
                //  source assigned by runtime
                content: BinSerializableMessageContent {
                    message_type: MessageType::Request(false),
                    payload: BinSerializablePayload {
                        json: Some(serde_json::to_vec(
                            &FileSystemRequest {
                                uri_string,
                                action: FileSystemAction::Write,
                            }
                        ).unwrap()),
                        bytes: Some(process_wasm_bytes),
                        // bytes: None,  //  TODO
                    },
                },
            }
        };
        boot_sequence.push(save_process_message);
    }

    //  TODO: race condition?

    //  start process_manager by messaging kernel directly
    let _ = processes_to_start.remove(0);
    let process = "process_manager";
    let wasm_bytes_uri = format!("fs://{}.wasm", process);
    let process_wasm_path =
        format!("../modules/{process}/target/wasm32-unknown-unknown/release/{process}.wasm");
    let process_wasm_bytes = fs::read(&process_wasm_path).await.expect(&process_wasm_path);
    let start_process_message = BinSerializableWrappedMessage {
        id: rand::random(),
        //  target node assigned by runtime to "our"
        target_process: "kernel".into(),
        //  rsvp assigned by runtime (as None)
        message: BinSerializableMessage {
            //  source assigned by runtime
            content: BinSerializableMessageContent {
                message_type: MessageType::Request(false),
                payload: BinSerializablePayload {
                    json: Some(serde_json::to_vec(
                        &KernelRequest::StartProcess {
                            process_name: (*process).into(),
                            wasm_bytes_uri,
                            send_on_panic: SendOnPanic::None,  //  TODO: enable Restart
                        }
                    ).unwrap()),
                    bytes: Some(process_wasm_bytes),
                },
            },
        }
    };
    boot_sequence.push(start_process_message);

    //  start other processes by messaging process_manager
    for process in &processes_to_start {
        let wasm_bytes_uri = format!("fs://{}.wasm", process);
        let start_process_message = BinSerializableWrappedMessage {
            id: rand::random(),
            //  target node assigned by runtime to "our"
            target_process: "process_manager".into(),
            //  rsvp assigned by runtime (as None)
            message: BinSerializableMessage {
                //  source assigned by runtime
                content: BinSerializableMessageContent {
                    message_type: MessageType::Request(false),
                    payload: BinSerializablePayload {
                        json: Some(serde_json::to_vec(
                            &ProcessManagerCommand::Start {
                                process_name: (*process).into(),
                                wasm_bytes_uri,
                                send_on_panic: SendOnPanic::Restart,
                            }
                        ).unwrap()),
                        bytes: None,
                    },
                },
            }
        };
        boot_sequence.push(start_process_message);
    }

    //  add new initialization Messages here
    //  e.g.
    //
    //  ```
    //  let foo = BinSerializableWrappedMessage { .. };
    //  boot_sequence.push(foo);
    //  ```

    boot_sequence
}

#[tokio::main]
async fn main() {
    let boot_sequence = pill().await;
    let serialized = bincode::serialize(&boot_sequence).unwrap();
    let _ = fs::write("../boot_sequence.bin", serialized).await;
}
