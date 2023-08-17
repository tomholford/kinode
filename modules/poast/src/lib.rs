cargo_component_bindings::generate!();

use bindings::component::microkernel_process::types;

struct Component;


impl bindings::MicrokernelProcess for Component {
    fn run_process(our: String, dap: String) {
        bindings::print_to_terminal(1, "poast: start");
        bindings::yield_results(
            vec![(
                bindings::WitProtomessage {
                    protomessage_type: types::WitProtomessageType::Request(
                        types::WitRequestTypeWithTarget {
                            is_expecting_response: false,
                            target_ship: our.as_str(),
                            target_app: "http_bindings",
                        }
                    ),
                    payload: &types::WitPayload {
                        json: Some(serde_json::json!({
                            "action": "bind-app",
                            "path": "/poast", // TODO at some point we need URL pattern matching...later...
                            "app": dap
                        }).to_string()),
                        bytes: None
                    }
                },
                "",
            ), (
                bindings::WitProtomessage {
                    protomessage_type: types::WitProtomessageType::Request(
                        types::WitRequestTypeWithTarget {
                            is_expecting_response: false,
                            target_ship: our.as_str(),
                            target_app: "http_bindings",
                        }
                    ),
                    payload: &types::WitPayload {
                        json: Some(serde_json::json!({
                            "action": "bind-app",
                            "path": "/poast/:banger", // TODO at some point we need URL pattern matching...later...
                            "app": dap
                        }).to_string()),
                        bytes: None
                    }
                },
                "",
            )].as_slice()
        );

        loop {
            let (message, _) = bindings::await_next_message();
            let Some(message_from_loop_string) = message.payload.json else {
                panic!("foo")
            };
            let message_from_loop: serde_json::Value = serde_json::from_str(&message_from_loop_string).unwrap();
            bindings::print_to_terminal(1, format!("poast: got request: {}", message_from_loop).as_str());
            bindings::print_to_terminal(1, format!("METHOD: {}", message_from_loop["method"]).as_str());
            if message_from_loop["method"] == "GET" && message_from_loop["path"] == "/poast" {
                bindings::yield_results(vec![(
                    bindings::WitProtomessage {
                        protomessage_type: types::WitProtomessageType::Response,
                        payload: &types::WitPayload {
                            json: Some(serde_json::json!({
                                "action": "response",
                                "status": 201,
                                "headers": {
                                    "Content-Type": "text/html",
                                },
                            }).to_string()),
                            bytes: Some("<h1>you just performed a GET to poast</h1>".as_bytes().to_vec())
                        }
                    },
                    "",
                )].as_slice());
            } else if message_from_loop["method"] == "POST" && message_from_loop["path"] == "/poast" {
                bindings::yield_results(vec![(
                    bindings::WitProtomessage {
                        protomessage_type: types::WitProtomessageType::Response,
                        payload: &types::WitPayload {
                            json: Some(serde_json::json!({
                                "action": "response",
                                "status": 201,
                                "headers": {
                                    "Content-Type": "application/json",
                                },
                            }).to_string()),
                            bytes: Some(format!(
                                "you just performed a POST with body: {:?}", String::from_utf8(message.payload.bytes.unwrap_or(vec![])
                            )).as_bytes().to_vec())
                        }
                    },
                    "",
                )].as_slice());
            } else if message_from_loop["method"] == "GET" && message_from_loop["path"] == "/poast/:banger" {
                let mut params = String::new();
                for (key, value) in message_from_loop["url_params"].as_object().unwrap() {
                    params.push_str(format!("{}={}&", key, value).as_str());
                }
                let mut query_params = String::new();
                for (key, value) in message_from_loop["query_params"].as_object().unwrap() {
                    query_params.push_str(format!("{}={}&", key, value).as_str());
                }
                bindings::yield_results(vec![(
                    bindings::WitProtomessage {
                        protomessage_type: types::WitProtomessageType::Response,
                        payload: &types::WitPayload {
                            json: Some(serde_json::json!({
                                "action": "response",
                                "status": 201,
                                "headers": {
                                    "Content-Type": "application/json",
                                },
                            }).to_string()),
                            bytes: Some(format!(
                                "you just performed a GET with url params {:?} and query params {:?}", params, query_params
                            ).as_bytes().to_vec())
                        }
                    },
                    "",
                )].as_slice());
            } else {
                bindings::yield_results(vec![(
                    bindings::WitProtomessage {
                        protomessage_type: types::WitProtomessageType::Response,
                        payload: &types::WitPayload {
                            json: Some(serde_json::json!({
                                "action": "response",
                                "status": 201,
                                "headers": {
                                    "Content-Type": "application/json",
                                },
                            }).to_string()),
                            bytes: Some("you made a request that was not GET or POST".as_bytes().to_vec())
                        }
                    },
                    "",
                )].as_slice());
            }
        }
    }
}
