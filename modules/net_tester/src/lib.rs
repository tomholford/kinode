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
    fn run_process(our: types::ProcessAddress) {
        bindings::print_to_terminal(0, "net_tester: init");
        loop {
            let (inbound, _) = bindings::receive().unwrap();
            let command = match inbound {
                types::InboundMessage::Request(request) => request.payload,
                _ => continue,
            };
            if command.source.node != our.node {
                bindings::print_to_terminal(
                    0,
                    format!(
                        "net_tester: got message {} from {}",
                        command.json.unwrap(),
                        command.source.node,
                    )
                    .as_str(),
                );
                continue;
            } else if let types::ProcessIdentifier::Name(name) = command.source.identifier {
                if name != "terminal" {
                    bindings::print_to_terminal(
                        0,
                        format!(
                            "net_tester: got message {} from {}",
                            command.json.unwrap(),
                            name,
                        )
                        .as_str(),
                    );
                    continue;
                }
                let command: Value = from_str(&command.json.unwrap()).unwrap();
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
                    messages.push((
                        vec![types::OutboundRequest {
                            is_expecting_response: false,
                            target: types::ProcessReference {
                                node: target.into(),
                                identifier: types::ProcessIdentifier::Name("net_tester".into()),
                            },
                            payload: types::OutboundPayload {
                                json: None,
                                bytes: types::OutboundPayloadBytes::Some(chunk.clone()),
                            },
                        }],
                        "".into(),
                    ));
                }
                bindings::send(Ok(&types::OutboundMessage::Requests(messages)));
                continue;
            }
        }
    }
}
