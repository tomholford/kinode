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
                            target_app: "http_bindings",
                        }
                    ),
                    payload: &WitPayload {
                        json: Some(serde_json::json!({
                            "action": "bind-app",
                            "path": "/poast", // TODO at some point we need URL pattern matching...later...
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
            let message_from_loop: serde_json::Value = serde_json::from_str(&message_from_loop_string).unwrap();
            bindings::print_to_terminal(format!("poast: got request: {}", message_from_loop).as_str());
            bindings::print_to_terminal(format!("ID: {}", message_from_loop["id"]).as_str());
            bindings::print_to_terminal(format!("METHOD: {}", message_from_loop["method"]).as_str());
            if message_from_loop["method"] == "GET" {
                bindings::yield_results(vec![
                    bindings::WitProtomessage {
                        protomessage_type: WitProtomessageType::Response,
                        payload: &WitPayload {
                            json: Some(serde_json::json!({
                                "action": "response",
                                "id": message_from_loop["id"],
                                "status": 201,
                                "headers": {
                                    "Content-Type": "text/html",
                                },
                            }).to_string()),
                            bytes: Some("<h1>you just performed a GET to poast</h1>".as_bytes().to_vec())
                        }
                    }
                ].as_slice());
            } else if message_from_loop["method"] == "POST" {
                bindings::yield_results(vec![
                    bindings::WitProtomessage {
                        protomessage_type: WitProtomessageType::Response,
                        payload: &WitPayload {
                            json: Some(serde_json::json!({
                                "action": "response",
                                "id": message_from_loop["id"],
                                "status": 201,
                                "headers": {
                                    "Content-Type": "application/json",
                                },
                            }).to_string()),
                            bytes: Some(format!(
                                "you just performed a POST with body: {:?}", String::from_utf8(message.payload.bytes.unwrap_or(vec![])
                            )).as_bytes().to_vec())
                        }
                    }
                ].as_slice());
            } else {
                bindings::yield_results(vec![
                    bindings::WitProtomessage {
                        protomessage_type: WitProtomessageType::Response,
                        payload: &WitPayload {
                            json: Some(serde_json::json!({
                                "action": "response",
                                "id": message_from_loop["id"],
                                "status": 201,
                                "headers": {
                                    "Content-Type": "application/json",
                                },
                            }).to_string()),
                            bytes: Some("you made a request that was not GET or POST".as_bytes().to_vec())
                        }
                    }
                ].as_slice());
            }
        }
    }
}

bindings::export!(Component);
