cargo_component_bindings::generate!();

use bindings::component::microkernel_process::types;
use serde_json::*;

struct Component;

/*
 *  sends a bunch of empty bytes across network
 *  each chunk is "size" field bytes large
 *  format: !message our net_tester {"chunks": 1, "size": 65536, "target": "tester3"}
 */
impl bindings::MicrokernelProcess for Component {
    fn run_process(our_name: String, _: String) {
        bindings::print_to_terminal(0, "net_tester: init");
        loop {
            let (command, _) = bindings::await_next_message().unwrap();
            if command.source.node != our_name {
                bindings::print_to_terminal(
                    0,
                    format!(
                        "net_tester: got message {} from {}",
                        command.content.payload.json.unwrap(),
                        command.source.node,
                    )
                    .as_str(),
                );
                continue;
            } else if command.source.process != "terminal" {
                bindings::print_to_terminal(
                    0,
                    format!(
                        "net_tester: got message from {}: {:?}",
                        command.source.process, command.content.payload.json,
                    )
                    .as_str(),
                );
                continue;
            }
            let command: Value = from_str(&command.content.payload.json.unwrap()).unwrap();
            // read size of transfer to test and do it
            bindings::print_to_terminal(
                0,
                format!("net_tester: got command {:?}", command).as_str(),
            );
            let chunks: u64 = command["chunks"].as_u64().unwrap();
            let chunk: Vec<u8> = vec![0xfu8; command["size"].as_u64().unwrap() as usize];
            let target = command["target"].as_str().unwrap();

            let mut messages = Vec::new();
            for num in 1..chunks + 1 {
                messages.push(types::WitProtorequest {
                    is_expecting_response: false,
                    target: types::WitProcessNode {
                        node: target.into(),
                        process: "net_tester".into(),
                    },
                    payload: types::WitPayload {
                        json: Some(num.to_string()),
                        bytes: types::WitPayloadBytes {
                            circumvent: types::WitCircumvent::False,
                            content: Some(chunk.clone()),
                        },
                    },
                });
            }
            bindings::send_requests(Ok((messages.as_slice(), "".into())));
        }
    }
}
