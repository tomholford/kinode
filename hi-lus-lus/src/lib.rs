use bindings::component::microkernel_process::types::WitProtomessageType;
use bindings::component::microkernel_process::types::WitRequestTypeWithTarget;

struct Component;

struct Messages {
    received: Vec<serde_json::Value>,
    sent: Vec<serde_json::Value>,
}

impl bindings::MicrokernelProcess for Component {
    fn run_process(_: String, _: String) {
        bindings::print_to_terminal(1, "hi++: start");

        let mut messages = Messages {
            received: vec![],
            sent: vec![],
        };

        loop {
            let (message, _) = bindings::await_next_message();
            let Some(message_from_loop_string) = message.payload.json else {
                panic!("foo")
            };
            let message_from_loop: serde_json::Value =
                serde_json::from_str(&message_from_loop_string).unwrap();
            if let serde_json::Value::String(action) = &message_from_loop["action"] {
                if action == "receive" {
                    messages.received.push(
                        serde_json::to_value(&message_from_loop_string).unwrap()
                    );
                    bindings::print_to_terminal(0,
                        format!(
                            "hi++: got message {}",
                            message_from_loop_string
                        ).as_str()
                    );
                } else if action == "send" {
                    messages.sent.push(
                        serde_json::to_value(&message_from_loop_string).unwrap()
                    );
                    let serde_json::Value::String(ref target) =
                        message_from_loop["target"] else { panic!("unexpected target") };
                    let serde_json::Value::String(ref contents) =
                        message_from_loop["contents"] else { panic!("unexpected contents") };
                    let payload = serde_json::json!({
                        "action": "receive",
                        "target": target,
                        "contents": contents,
                    });
                    let response = bindings::component::microkernel_process::types::WitPayload {
                        json: Some(payload.to_string()),
                        bytes: None,
                    };
                    bindings::yield_results(
                        vec![
                            (
                                bindings::WitProtomessage {
                                    protomessage_type: WitProtomessageType::Request(
                                        WitRequestTypeWithTarget {
                                            is_expecting_response: false,
                                            target_ship: target,
                                            target_app: "hi_lus_lus",
                                        }
                                    ),
                                    payload: &response,
                                },
                                "",
                            )
                        ].as_slice()
                    );
                } else {
                    bindings::print_to_terminal(0,
                        format!(
                            "hi++: unexpected action (expected either 'send' or 'receive'): {:?}",
                            &message_from_loop["action"],
                        ).as_str()
                    );
                }
            } else {
                bindings::print_to_terminal(0,
                    format!(
                        "hi++: unexpected action: {:?}",
                        &message_from_loop["action"],
                    ).as_str()
                );
            }
        }
    }
}

bindings::export!(Component);
