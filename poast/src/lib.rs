use serde_json::json;

struct Component;

impl bindings::MicrokernelProcess for Component {
    fn from_event_loop(our: String, message_from_loop: String) {
        let mut response = "\"".to_string();
        response.push_str(&message_from_loop);
        response.push_str(" appended by poast\"");
        let state_string = bindings::fetch_state("");
        let mut state = serde_json::from_str(&state_string).unwrap();
        state = match state {
            serde_json::Value::Null => {
                json!([response.clone()])
            },
            serde_json::Value::Array(mut vector) => {
                vector.push(serde_json::to_value(response.clone()).unwrap());
                serde_json::Value::Array(vector)
            },
            _ => json!([response.clone()])  // TODO
        };
        bindings::set_state(serde_json::to_string(&state).unwrap().as_str());
        bindings::to_event_loop(&our, "http_server", &response);
    }
}

bindings::export!(Component);
