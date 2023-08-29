cargo_component_bindings::generate!();

use bindings::component::microkernel_process::types;

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
            bindings::send_requests(Ok((
                vec![
                    types::WitProtorequest {
                        is_expecting_response: false,
                        target: types::WitProcessReference {
                            node: target.into(),
                            identifier: types::WitProcessIdentifier::Name("net".into()),
                        },
                        payload: types::WitPayload {
                            json: Some(serde_json::Value::String(message.into()).to_string()),
                            bytes: types::WitPayloadBytes {
                                circumvent: types::WitCircumvent::False,
                                content: None,
                            },
                        },
                    },
                ].as_slice(),
                "".into(),
            )));
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
            bindings::send_requests(Ok((
                vec![
                    types::WitProtorequest {
                        is_expecting_response: false,
                        target: types::WitProcessReference {
                            node: if target_node == "our" {
                                our_name.into()
                            } else {
                                target_node.into()
                            },
                            identifier: types::WitProcessIdentifier::Name(
                                target_process.into()
                            ),
                        },
                        payload: types::WitPayload {
                            json: Some(payload.into()),
                            bytes: types::WitPayloadBytes {
                                circumvent: types::WitCircumvent::False,
                                content: None,
                            },
                        },
                    },
                ].as_slice(),
                "".into(),
            )));
        }
        _ => {
            bindings::print_to_terminal(1, &format!("invalid command: \"{line}\""));
        }
    }
}

impl bindings::MicrokernelProcess for Component {
    fn run_process(our: types::WitProcessAddress) {
    // fn run_process(our_name: String, process_name: String) {
        let Some(ref process_name) = our.name else {
            bindings::print_to_terminal(0, "terminal: require our.name set");
            panic!();
        };
        assert_eq!(process_name, "terminal");
        bindings::print_to_terminal(1, format!("{:?} terminal: running", our).as_str());

        loop {
            let (message, _) = bindings::await_next_message().unwrap();  //  TODO: handle error properly
            if let types::WitMessageType::Request(_) = message.content.message_type {
                let stringy = bincode::deserialize(&message.content.payload.bytes.content.unwrap_or_default())
                    .unwrap_or_default();
                parse_command(&our.node, stringy);
            } else {
                if let Some(s) = message.content.payload.json {
                    bindings::print_to_terminal(0, &format!("net error: {}!", &s));
                }
            }
        }
    }
}
