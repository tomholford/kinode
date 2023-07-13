use serde_json::json;

struct Component;

impl bindings::MicrokernelProcess for Component {
    fn init(_source: bindings::WitAppNode) {
        bindings::set_state(serde_json::to_string(&json!([])).unwrap().as_str());
    }

    fn run_write(message: bindings::WitMessage) {
        let bindings::component::microkernel_process::types::WitPayload::Json(message_from_loop) = message.payload else {
            panic!("foo")
        };
        let mut response_string = "\"".to_string();
        response_string.push_str(&message_from_loop);
        response_string.push_str(" appended by poast\"");
        let response = bindings::component::microkernel_process::types::WitPayload::Json(response_string.clone());
        let state_string = bindings::fetch_state("");
        let mut state = serde_json::from_str(&state_string).unwrap();
        state = match state {
            serde_json::Value::Array(mut vector) => {
                vector.push(serde_json::to_value(response_string.clone()).unwrap());
                serde_json::Value::Array(vector)
            },
            _ => json!([response_string.clone()])  // TODO
        };
        bindings::set_state(serde_json::to_string(&state).unwrap().as_str());
        bindings::to_event_loop(
            &bindings::WitAppNode {
                server: message.source.server.clone(),
                app: "http_server".to_string(),
            },
            &response
        );
    }

    fn run_read(_message: bindings::WitMessage) -> String {
        "".to_string()
    }

    fn run_take(_message: bindings::WitMessage) {
        bindings::print_to_terminal("in take");
    }
}

bindings::export!(Component);
