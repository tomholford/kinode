use serde_json::json;

struct Component;

impl bindings::MicrokernelProcess for Component {
    fn init(_our: String) {
        bindings::set_state(serde_json::to_string(&json!([])).unwrap().as_str());
    }

    fn run_write(our: String, message_from_loop: String) {
        let mut response = "\"".to_string();
        response.push_str(&message_from_loop);
        response.push_str(" appended by poast\"");
        let state_string = bindings::fetch_state("");
        let mut state = serde_json::from_str(&state_string).unwrap();
        state = match state {
            serde_json::Value::Array(mut vector) => {
                vector.push(serde_json::to_value(response.clone()).unwrap());
                serde_json::Value::Array(vector)
            },
            _ => json!([response.clone()])  // TODO
        };
        bindings::set_state(serde_json::to_string(&state).unwrap().as_str());
        bindings::to_event_loop(&our, "http_server", &response);
    }

    fn run_read(_our: String, _message_from_loop: String) -> String {
        "".to_string()
    }
}

bindings::export!(Component);
