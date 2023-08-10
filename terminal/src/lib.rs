cargo_component_bindings::generate!();

use bindings::component::microkernel_process::types::{
    WitMessageType, WitPayload, WitProcessNode, WitProtomessage, WitProtomessageType, WitRequestTypeWithTarget,
};

struct Component;

fn parse_command(our_name: &str, line: String) {
    let (head, tail) = line.split_once(" ").unwrap_or((&line, ""));
    match head {
        "" | " " => {}
        "!hi" => {
            let (target, message) = match tail.split_once(" ") {
                Some((s, t)) => (s, t),
                None => {
                    bindings::print_to_terminal(1, &format!("invalid command: \"{}\"", line));
                    panic!("invalid command");
                }
            };
            bindings::yield_results(Ok(
                vec![(
                    WitProtomessage {
                        protomessage_type: WitProtomessageType::Request(WitRequestTypeWithTarget {
                            is_expecting_response: false,
                            target: WitProcessNode {
                                node: target.into(),
                                process: "net".into(),
                            },
                        }),
                        payload: WitPayload {
                            json: Some(serde_json::Value::String(message.into()).to_string()),
                            bytes: None,
                        },
                    },
                    "".into(),
                )]
                .as_slice(),
            ));
        }
        "!message" => {
            let (target_node, tail) = match tail.split_once(" ") {
                Some((s, t)) => (s, t),
                None => {
                    bindings::print_to_terminal(1, &format!("invalid command: \"{}\"", line));
                    panic!("invalid command");
                }
            };
            let (target_process, payload) = match tail.split_once(" ") {
                Some((a, p)) => (a, p),
                None => {
                    bindings::print_to_terminal(1, &format!("invalid command: \"{}\"", line));
                    panic!("invalid command");
                }
            };
            bindings::yield_results(Ok(
                vec![(
                    WitProtomessage {
                        protomessage_type: WitProtomessageType::Request(WitRequestTypeWithTarget {
                            is_expecting_response: false,
                            target: WitProcessNode {
                                node: if target_node == "our" {
                                    our_name.into()
                                } else {
                                    target_node.into()
                                },
                                process: target_process.into(),
                            },
                        }),
                        payload: WitPayload {
                            json: Some(payload.into()),
                            bytes: None,
                        },
                    },
                    "".into(),
                )]
                .as_slice(),
            ));
        }
        _ => {
            bindings::print_to_terminal(1, &format!("invalid command: \"{line}\""));
        }
    }
}

impl bindings::MicrokernelProcess for Component {
    fn run_process(our_name: String, process_name: String) {
        assert_eq!(process_name, "terminal");
        bindings::print_to_terminal(1, format!("{our_name} terminal: running").as_str());

        loop {
            let (message, _) = bindings::await_next_message().unwrap();  //  TODO: handle error properly
            if let WitMessageType::Request(_) = message.content.message_type {
                let stringy = bincode::deserialize(&message.content.payload.bytes.unwrap_or_default())
                    .unwrap_or_default();
                parse_command(&our_name, stringy);
            } else {
                if let Some(s) = message.content.payload.json {
                    bindings::print_to_terminal(0, &format!("net error: {}!", &s));
                }
            }
        }
    }
}
