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

    fn run_write(our: String, message_from_loop_string: String) {
        let message_from_loop = serde_json::from_str(&message_from_loop_string).unwrap();
        let contents = message_from_loop["contents"];
        if let serde_json::Value::String(action) = message_from_loop["action"] {
            match action {
                "receive" => {
                    let json_pointer = "/messages/received";
                    let state_string = bindings::fetch_state(json_pointer);
                    let mut state = serde_json::from_str(&state_string).unwrap();
                    if let serde_json::Value::Array(vector) = state {
                        vector.push(serde_json::to_value().unwrap());
                        bindings::modify_state(
                            json_pointer,
                            serde_json::to_string(serde_json::Value::Array(vector))
                        );
                    }
                    //  TODO: output
                },
                "send" => {
                    let json_pointer = "/messages/sent";
                    let state_string = bindings::fetch_state(json_pointer);
                    let mut state = serde_json::from_str(&state_string).unwrap();
                    if let serde_json::Value::Array(vector) = state {
                        vector.push(serde_json::to_value().unwrap());
                        bindings::modify_state(
                            json_pointer,
                            serde_json::to_string(serde_json::Value::Array(vector))
                        );
                    }
                    //  TODO: send message
                    let target = if let serde_json::Value::String(target) = message_from_loop["target"] { target };
                    let response = "";
                    bindings::to_event_loop(target, "hi_lus_lus", &response);
                }
            }
        }
    }

    fn run_read(our: String, message_from_loop: String) -> String {
        "".to_string()
    }
}

bindings::export!(Component);
