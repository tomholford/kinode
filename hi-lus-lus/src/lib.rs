use serde_json::json;

struct Component;

impl bindings::MicrokernelProcess for Component {
    fn init(_our: String) {
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
    }

    fn run_write(_our: String, message_from_loop_string: String) {
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
                    "contents": contents
                });
                bindings::to_event_loop(target, "hi_lus_lus", &payload.to_string());
            }
        } else {
            bindings::print_to_terminal(
                format!(
                    "hi++: unexpected action: {:?}",
                    &message_from_loop["action"]
                ).as_str()
            );
        }
    }

    fn run_read(_our: String, _message_from_loop: String) -> String {
        "".to_string()
    }
}

bindings::export!(Component);
