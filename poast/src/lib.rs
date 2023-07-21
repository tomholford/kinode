use bindings::component::microkernel_process::types::WitProtomessageType;
use bindings::component::microkernel_process::types::WitRequestTypeWithTarget;
use bindings::component::microkernel_process::types::WitPayload;

struct Component;


impl bindings::MicrokernelProcess for Component {
    fn run_process(our: String, dap: String) {
        bindings::print_to_terminal("poast: start");
        bindings::yield_results(
            vec![
                bindings::WitProtomessage {
                    protomessage_type: WitProtomessageType::Request(
                        WitRequestTypeWithTarget {
                            is_expecting_response: false,
                            target_ship: our.as_str(),
                            target_app: "http_server",
                        }
                    ),
                    payload: &WitPayload {
                        json: Some(serde_json::json!({
                                "path": "poast",
                                "app": dap
                        }).to_string()),
                        bytes: None
                    }

                },
            ].as_slice()
        );

        loop {
            let mut message_stack = bindings::await_next_message();
            let message = message_stack.pop().unwrap();
            let Some(message_from_loop_string) = message.payload.json else {
                panic!("foo")
            };
            let message_from_loop: serde_json::Value =
                serde_json::from_str(&message_from_loop_string).unwrap();
            bindings::print_to_terminal(format!("poast: got request: {}", message_from_loop).as_str())
        }
    }
}

bindings::export!(Component);
