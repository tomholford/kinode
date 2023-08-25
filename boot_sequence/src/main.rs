mod types;
use crate::types::*;
use tokio::fs;

fn make_sequentialize_request(
    json: Option<String>,
    bytes: TransitPayloadBytes,
) -> BootOutboundRequest {
    BootOutboundRequest {
        target_process: ProcessIdentifier::Name("sequentialize".into()),
        json,
        bytes,
    }
}

async fn start_process_via_kernel(process: &str, id: u64) -> BootOutboundRequest {
    let wasm_bytes_uri = format!("fs://sequentialize/{}.wasm", process);
    let process_wasm_path =
        format!("../modules/{process}/target/wasm32-unknown-unknown/release/{process}.wasm");
    let process_wasm_bytes = fs::read(&process_wasm_path).await.expect(&process_wasm_path);
    BootOutboundRequest {
        target_process: ProcessIdentifier::Name("kernel".into()),
        json: Some(serde_json::to_string(
            &KernelRequest::StartProcess {
                id,
                name: Some((*process).into()),
                wasm_bytes_uri,
                send_on_panic: SendOnPanic::None,  //  TODO: enable Restart
            }
        ).unwrap()),
        bytes: TransitPayloadBytes::Some(process_wasm_bytes),
    }
}

async fn save_bytes(process: &str) -> BootOutboundRequest {
    let uri_string = format!("fs://sequentialize/{}.wasm", process);
    let process_wasm_path =
        format!("../modules/{process}/target/wasm32-unknown-unknown/release/{process}.wasm");
    let process_wasm_bytes = fs::read(&process_wasm_path).await.expect(&process_wasm_path);
    make_sequentialize_request(
        Some(serde_json::to_string(&SequentializeRequest::QueueMessage {
            target_node: None,
            target_process: ProcessIdentifier::Name("filesystem".into()),
            json: Some(serde_json::to_string(&FileSystemRequest {
                uri_string,
                action: FileSystemAction::Write,
            }).unwrap()),
        }).unwrap()),
        TransitPayloadBytes::Some(process_wasm_bytes),
    )
}

fn start_process_via_pm(process: &str) -> BootOutboundRequest {
    let wasm_bytes_uri = format!("fs://sequentialize/{}.wasm", process);  //  TODO: how to get wasm files to top-level?
    make_sequentialize_request(
        Some(serde_json::to_string(&SequentializeRequest::QueueMessage {
            target_node: None,
            target_process: ProcessIdentifier::Name("process_manager".into()),
            json: Some(serde_json::to_string(&ProcessManagerCommand::Start {
                name: Some((*process).into()),
                wasm_bytes_uri,
                send_on_panic: SendOnPanic::Restart,
            }).unwrap()),
        }).unwrap()),
        TransitPayloadBytes::None,
    )
}

pub async fn pill() -> Vec<BootOutboundRequest> {
    //  add new processes_to_start here
    let mut processes_to_start = vec![
        "process_manager",
        "sequentialize",
        "terminal",
        "http_bindings",
        "apps_home",
        "http_proxy",
        // "file_transfer",
        "persist",
    ];

    let mut boot_sequence: Vec<BootOutboundRequest> = Vec::new();

    //  start by messaging kernel directly
    boot_sequence.push(start_process_via_kernel("process_manager", PROCESS_MANAGER_ID).await);
    boot_sequence.push(start_process_via_kernel("sequentialize", rand::random()).await);

    //  initialize boot sequence
    boot_sequence.push(make_sequentialize_request(
        Some(serde_json::to_string(&SequentializeRequest::QueueMessage {
            target_node: None,
            target_process: ProcessIdentifier::Name("process_manager".into()),
            json: Some(serde_json::to_string(&ProcessManagerCommand::Initialize {
                jwt_secret_bytes: None,
            }).unwrap()),
        }).unwrap()),
        TransitPayloadBytes::None,
    ));

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
