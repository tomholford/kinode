use serde_json::json;

struct Component;

impl bindings::MicrokernelProcess for Component {
    fn init(_source: bindings::WitAppNode) {
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

    fn run_write(message: bindings::WitMessage) {
        let bindings::component::microkernel_process::types::WitPayload::Json(
            message_from_loop_string
        ) = message.payload else {
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
                let wit_payload =
                    bindings::component::microkernel_process::types::WitPayload::Json(
                        payload.to_string()
                    );
                bindings::to_event_loop(
                    &bindings::WitAppNode {
                        server: target.to_string(),
                        app: "hi_lus_lus".to_string(),
                    },
                    &wit_payload
                );
            }
        } else {
            bindings::print_to_terminal(
                format!(
                    "hi++: unexpected action: {:?}",
                    &message_from_loop["action"],
                ).as_str()
            );
        }
    }

    fn run_read(_message: bindings::WitMessage) -> String {
        "".to_string()
    }

    fn run_take(_message: bindings::WitMessage) {
        bindings::print_to_terminal("in take");
    }
}

bindings::export!(Component);
