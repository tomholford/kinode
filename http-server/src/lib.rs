struct Component;

impl bindings::MicrokernelProcess for Component {
    fn init(_source: bindings::WitAppNode) {
    }

    fn run_write(message: bindings::WitMessage) {
        let Some(message_from_loop) = message.payload.json else {
            panic!("foo")
        };
        let mut response_string = "\"".to_string();
        response_string.push_str(&message_from_loop);
        response_string.push_str(" appended by http-server\"");
        let response = bindings::component::microkernel_process::types::WitPayload {
            json: Some(response_string),
            bytes: None,
        };
        bindings::print_to_terminal(format!("http_server: {:?}", response).as_str());
    }

    fn run_read(_message: bindings::WitMessage) -> String {
        "".to_string()
    }
}

bindings::export!(Component);
