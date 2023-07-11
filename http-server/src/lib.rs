struct Component;

impl bindings::MicrokernelProcess for Component {
    fn init(_source: bindings::WitAppNode) {
    }

    fn run_write(message: bindings::WitMessage) {
        // let bindings::WitPayload::Json(message_from_loop) = message.payload else {
        let bindings::component::microkernel_process::types::WitPayload::Json(message_from_loop) = message.payload else {
            panic!("foo")
        };
        let mut response_string = "\"".to_string();
        response_string.push_str(&message_from_loop);
        response_string.push_str(" appended by http-server\"");
        // let response = bindings::WitPayload::Json(response_string);
        let response = bindings::component::microkernel_process::types::WitPayload::Json(response_string);
        bindings::to_event_loop(&message.source, &response);
    }

    fn run_read(_message: bindings::WitMessage) -> String {
        "".to_string()
    }
}

bindings::export!(Component);
