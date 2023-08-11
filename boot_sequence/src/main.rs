mod types;
use crate::types::*;
use tokio::fs;

fn make_sequentialize_bswm(payload: BinSerializablePayload) -> BinSerializableWrappedMessage {
    BinSerializableWrappedMessage {
        id: rand::random(),
        //  target node assigned by runtime to "our"
        target_process: "sequentialize".into(),
        //  rsvp assigned by runtime (as None)
        message: BinSerializableMessage {
            //  source assigned by runtime
            content: BinSerializableMessageContent {
                message_type: MessageType::Request(false),
                payload,
            },
        }
    }
}

async fn start_process_via_kernel(process: &str) -> BinSerializableWrappedMessage {
    let wasm_bytes_uri = format!("fs://{}.wasm", process);
    let process_wasm_path =
        format!("../modules/{process}/target/wasm32-unknown-unknown/release/{process}.wasm");
    let process_wasm_bytes = fs::read(&process_wasm_path).await.expect(&process_wasm_path);
    BinSerializableWrappedMessage {
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
    }
}

async fn save_bytes(process: &str) -> BinSerializableWrappedMessage {
    let uri_string = format!("fs://{}.wasm", process);
    let process_wasm_path =
        format!("../modules/{process}/target/wasm32-unknown-unknown/release/{process}.wasm");
    let process_wasm_bytes = fs::read(&process_wasm_path).await.expect(&process_wasm_path);
    make_sequentialize_bswm(BinSerializablePayload {
        json: Some(serde_json::to_vec(&SequentializeRequest::QueueMessage {
            target_node: None,
            target_process: "filesystem".into(),
            json: Some(serde_json::to_string(&FileSystemRequest {
                uri_string,
                action: FileSystemAction::Write,
            }).unwrap()),
        }).unwrap()),
        bytes: Some(process_wasm_bytes),
    })
}

fn start_process_via_pm(process: &str) -> BinSerializableWrappedMessage {
    let wasm_bytes_uri = format!("fs://sequentialize/{}.wasm", process);  //  TODO: how to get wasm files to top-level?
    // let wasm_bytes_uri = format!("fs://{}.wasm", process);
    make_sequentialize_bswm(BinSerializablePayload {
        json: Some(serde_json::to_vec(&SequentializeRequest::QueueMessage {
            target_node: None,
            target_process: "process_manager".into(),
            json: Some(serde_json::to_string(&ProcessManagerCommand::Start {
                process_name: (*process).into(),
                wasm_bytes_uri,
                send_on_panic: SendOnPanic::Restart,
            }).unwrap()),
        }).unwrap()),
        bytes: None,
    })
}

pub async fn pill() -> Vec<BinSerializableWrappedMessage> {
    //  add new processes_to_start here
    let mut processes_to_start = vec![
        "process_manager",
        "sequentialize",
        "terminal",
        "http_bindings",
        "apps_home",
        "http_proxy",
        "file_transfer",
    ];

    let mut boot_sequence: Vec<BinSerializableWrappedMessage> = Vec::new();

    //  start by messaging kernel directly
    boot_sequence.push(start_process_via_kernel("process_manager").await);
    boot_sequence.push(start_process_via_kernel("sequentialize").await);

    //  copy wasm bytes into home dir
    for process in &processes_to_start {
        boot_sequence.push(save_bytes(process).await);
    }

    let _ = processes_to_start.drain(0..2);

    //  start other processes by messaging process_manager
    for process in &processes_to_start {
        boot_sequence.push(start_process_via_pm(process));
    }

    //  add new initialization Messages here
    //  e.g.
    //
    //  ```
    //  let foo = BinSerializableWrappedMessage { .. };
    //  boot_sequence.push(foo);
    //  ```

    //  it is runtime's responsibility to run the sequentialize queue

    boot_sequence
}

#[tokio::main]
async fn main() {
    let boot_sequence = pill().await;
    let serialized = bincode::serialize(&boot_sequence).unwrap();
    let _ = fs::write("../boot_sequence.bin", serialized).await;
}
