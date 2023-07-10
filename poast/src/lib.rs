struct Component;

impl bindings::MicrokernelProcess for Component {
    fn from_event_loop(our: String, message_from_loop: String) {
        let mut response = "\"".to_string();
        response.push_str(&message_from_loop);
        response.push_str(" appended by poast\"");
        bindings::to_event_loop(&our, "http_server", &response);
    }
}

bindings::export!(Component);
