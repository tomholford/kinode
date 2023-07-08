struct Component;

impl bindings::MicrokernelProcess for Component {
    fn init(_our: String) {
    }

    fn run_write(our: String, message_from_loop: String) {
        let mut response = "\"".to_string();
        response.push_str(&message_from_loop);
        response.push_str(" appended by http-server\"");
        bindings::to_event_loop(&our, "earth", &response);
    }

    fn run_read(_our: String, _message_from_loop: String) -> String {
        "".to_string()
    }
}

bindings::export!(Component);
