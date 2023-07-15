use serde_json::json;
use bindings::component::microkernel_process::types::WitRequestTypeWithTarget;

struct Component;

impl bindings::MicrokernelProcess for Component {
    fn init(_source_ship: String, _source_app: String) -> Vec<bindings::WitMessage> {
        bindings::set_state(
            serde_json::to_string(
                &json!({
                    "messages": {
                        "received": [],
                        "sent": []
                    }
                })
            )
            .unwrap()
            .as_str()
        );
        vec![]
    }

    fn run_write(
        mut message_stack: Vec<bindings::WitMessage>
    ) -> Vec<(bindings::WitMessageTypeWithTarget, bindings::WitPayload)> {
        let message = message_stack.pop().unwrap();
        let Some(message_from_loop_string) = message.payload.json else {
            panic!("foo")
        };
        let message_from_loop: serde_json::Value =
            serde_json::from_str(&message_from_loop_string).unwrap();
        if let serde_json::Value::String(action) = &message_from_loop["action"] {
            if action == "receive" {
                let json_pointer = "/messages/received";
                let state_string = bindings::fetch_state(json_pointer);
                let state = serde_json::from_str(&state_string).unwrap();
                if let serde_json::Value::Array(mut vector) = state {
                    vector.push(serde_json::to_value(&message_from_loop_string).unwrap());
                    bindings::modify_state(
                        json_pointer,
                        serde_json::to_string(&serde_json::Value::Array(vector))
                            .unwrap()
                            .as_str()
                    );
                }
                bindings::print_to_terminal(
                    format!(
                        "hi++: got message {}",
                        message_from_loop_string
                    ).as_str()
                );
                vec![]
            } else if action == "send" {
                let json_pointer = "/messages/sent";
                let state_string = bindings::fetch_state(json_pointer);
                let state = serde_json::from_str(&state_string).unwrap();
                if let serde_json::Value::Array(mut vector) = state {
                    vector.push(serde_json::to_value(&message_from_loop_string).unwrap());
                    bindings::modify_state(
                        json_pointer,
                        serde_json::to_string(&serde_json::Value::Array(vector))
                            .unwrap()
                            .as_str()
                    );
                }
                let serde_json::Value::String(ref target) =
                    message_from_loop["target"] else { panic!("unexpected target") };
                let serde_json::Value::String(ref contents) =
                    message_from_loop["contents"] else { panic!("unexpected contents") };
                let payload = json!({
                    "action": "receive",
                    "target": target,
                    "contents": contents,
                });
                let response = bindings::component::microkernel_process::types::WitPayload {
                    json: Some(payload.to_string()),
                    bytes: None,
                };
                // bindings::to_event_loop(
                //     &target.to_string(),
                //     &"hi_lus_lus".to_string(),
                //     bindings::WitMessageType::Request(false),
                //     &response,
                // );
                vec![(
                    bindings::WitMessageTypeWithTarget::Request(
                        WitRequestTypeWithTarget {
                            is_expecting_response: false,
                            target_ship: target.to_string(),
                            target_app: "hi_lus_lus".to_string(),
                        }
                    ),
                    response,
                )]
            } else {
                bindings::print_to_terminal(
                    format!(
                        "hi++: unexpected action (expected either 'send' or 'receive'): {:?}",
                        &message_from_loop["action"],
                    ).as_str()
                );
                vec![]
            }
        } else {
            bindings::print_to_terminal(
                format!(
                    "hi++: unexpected action: {:?}",
                    &message_from_loop["action"],
                ).as_str()
            );
            vec![]
        }


    }

    fn run_read(
        _message_stack: Vec<bindings::WitMessage>
    ) -> Vec<(bindings::WitMessageType, bindings::WitPayload)> {
        vec![]
    }

    fn handle_response(
        _message_stack: Vec<bindings::WitMessage>
    ) -> Vec<(bindings::WitMessageTypeWithTarget, bindings::WitPayload)> {
        bindings::print_to_terminal("in take");
        vec![]
    }
}

bindings::export!(Component);
