struct Component;

impl bindings::MicrokernelProcess for Component {
    fn init(_source_ship: String, _source_app: String) -> Vec<bindings::WitMessage> {
        vec![]
    }

    fn run_write(
        mut message_stack: Vec<bindings::WitMessage>
    ) -> Vec<(bindings::WitMessageTypeWithTarget, bindings::WitPayload)> {
        let message = message_stack.pop().unwrap();
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
        vec![]
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
