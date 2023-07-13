struct Component;

impl bindings::MicrokernelProcess for Component {
    fn init(_source_ship: String, _source_app: String) {
    }

    fn run_write(message: bindings::WitMessage) {
        let bindings::component::microkernel_process::types::WitPayload::Json(message_from_loop) = message.payload else {
            panic!("foo")
        };
        let mut response_string = "\"".to_string();
        response_string.push_str(&message_from_loop);
        response_string.push_str(" appended by http-server\"");
        let response = bindings::component::microkernel_process::types::WitPayload::Json(response_string);
        bindings::print_to_terminal(format!("http_server: {:?}", response).as_str());
    }

    fn run_read(_message: bindings::WitMessage) -> String {
        "".to_string()
    }

    fn run_take(_message: bindings::WitMessage) {
        bindings::print_to_terminal("in take");
    }
}

bindings::export!(Component);
