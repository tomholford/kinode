mod types;
use crate::types::*;
use tokio::fs;
use std::io;
use std::process::Command;

fn run_command(cmd: &mut Command) -> io::Result<()> {
    let status = cmd.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::Other, "Command failed"))
    }
}

pub async fn pill() -> Vec<BinSerializableWrappedMessage> {
    //  add new processes_to_start here, and symlink in src/
    let processes_to_start = vec![
        "process_manager",
        "terminal",
        "http_bindings",
        "apps_home",
        "http_proxy",
        "file_transfer",
    ];

    let mut boot_sequence: Vec<BinSerializableWrappedMessage> = Vec::new();

    for process in &processes_to_start {
        let process_wasm_path = format!("{process}.wasm");
        // will fail if symlink already exists, which is good
        let _ = run_command(
            Command::new("ln")
                .args([
                    "-s",
                    &format!("../modules/{process}/target/wasm32-unknown-unknown/release/{process}.wasm"),
                    &process_wasm_path,
                ])
        );
        let process_wasm_bytes = fs::read(&process_wasm_path).await.expect(&process_wasm_path);
        let start_process_message = BinSerializableWrappedMessage {
            id: rand::random(),
            //  target assigned by runtime
            //  rsvp assigned by runtime (as None)
            message: BinSerializableMessage {
                //  source assigned by runtime
                content: BinSerializableMessageContent {
                    message_type: MessageType::Request(false),
                    payload: BinSerializablePayload {
                        json: Some(serde_json::to_vec(
                            &KernelRequest::StartProcess(
                                ProcessStart{
                                    process_name: (*process).into(),
                                    wasm_bytes_uri: process_wasm_path,
                                }
                            )
                        ).unwrap()),
                        bytes: Some(process_wasm_bytes),
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
