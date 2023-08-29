cargo_component_bindings::generate!();

use bindings::{MicrokernelProcess, print_to_terminal, receive};
use bindings::component::microkernel_process::types;

mod process_lib;

struct Component;

fn parse_command(our_name: &str, line: String) {
    let (head, tail) = line.split_once(" ").unwrap_or((&line, ""));
    match head {
        "" | " " => {}
        "!hi" => {
            let (target, message) = match tail.split_once(" ") {
                Some((s, t)) => (s, t),
                None => {
                    print_to_terminal(1, &format!("invalid command: \"{}\"", line));
                    panic!("invalid command");
                }
            };
            let _ = process_lib::send_one_request(
                false,
                target.into(),
                types::ProcessIdentifier::Name("net".into()),
                Some(serde_json::Value::String(message.into())),
                types::OutboundPayloadBytes::None,
                None::<String>,
            );
        }
        "!message" => {
            let (target_node, tail) = match tail.split_once(" ") {
                Some((s, t)) => (s, t),
                None => {
                    print_to_terminal(1, &format!("invalid command: \"{}\"", line));
                    panic!("invalid command");
                }
            };
            let (target_process, payload) = match tail.split_once(" ") {
                Some((a, p)) => (a, p),
                None => {
                    print_to_terminal(1, &format!("invalid command: \"{}\"", line));
                    panic!("invalid command");
                }
            };
            //  TODO: why does this work but using the API below does not?
            //        Is it related to passing json in rather than a Serialize type?
            bindings::send(Ok(&types::OutboundMessage::Requests(vec![(
                vec![
                    types::OutboundRequest {
                        is_expecting_response: false,
                        target: types::ProcessReference {
                            node:
                                if target_node == "our" {
                                    our_name.into()
                                } else {
                                    target_node.into()
                                },
                            identifier: types::ProcessIdentifier::Name(target_process.into()),
                        },
                        payload: types::OutboundPayload {
                            json: Some(payload.into()),
                            bytes: types::OutboundPayloadBytes::None,
                        },
                    },
                ],
                "".into(),
            )])));
            // let json = serde_json::to_value(payload).unwrap();
            // print_to_terminal(1, &format!("terminal: got {}", json));
            // let _ = process_lib::send_one_request(
            //     false,
            //     if target_node == "our" {
            //         our_name.into()
            //     } else {
            //         target_node.into()
            //     },
            //     types::ProcessIdentifier::Name(target_process.into()),
            //     Some(json),
            //     types::OutboundPayloadBytes::None,
            //     None::<String>,
            // );
        }
        _ => {
            print_to_terminal(1, &format!("invalid command: \"{line}\""));
        }
    }
}

impl MicrokernelProcess for Component {
    fn run_process(our: types::ProcessAddress) {
        let Some(ref process_name) = our.name else {
            print_to_terminal(0, "terminal: require our.name set");
            panic!();
        };
        assert_eq!(process_name, "terminal");
        print_to_terminal(1, format!("{:?} terminal: running", our).as_str());

        loop {
            let (message, _) = receive().unwrap();  //  TODO: handle error properly
            match message {
                types::InboundMessage::Request(types::InboundRequest {
                    is_expecting_response: _,
                    payload: types::InboundPayload {
                        source: _,
                        json: _,
                        ref bytes,
                    },
                }) => {
                    let types::InboundPayloadBytes::Some(bytes) = bytes else {
                        continue;
                    };
                    let stringy = bincode::deserialize(bytes)
                        .unwrap_or_default();
                    parse_command(&our.node, stringy);
                },
                types::InboundMessage::Response(types::InboundPayload {
                    source: _,
                    ref json,
                    bytes: _,
                }) => {
                    if let Some(s) = json {
                        print_to_terminal(0, &format!("net error: {}!", &s));
                    }
                },
            }
        }
    }
}
