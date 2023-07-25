use bindings::component::microkernel_process::types::WitProtomessageType;
use bindings::component::microkernel_process::types::WitMessageType;
use bindings::component::microkernel_process::types::WitRequestTypeWithTarget;
use bindings::component::microkernel_process::types::WitPayload;
use std::collections::HashMap;

struct Component;

impl bindings::MicrokernelProcess for Component {
    fn run_process(our: String, dap: String) {
        bindings::print_to_terminal("http_bindings: start");
        // TODO needs to be some kind of HttpPath => String
        let mut bindings: HashMap<String, String> = HashMap::new();
        
        loop {
            let mut message_stack = bindings::await_next_message();
            let message = message_stack.pop().unwrap();
            let Some(message_json_text) = message.payload.json else {
                panic!("foo")
            };
            let message_json: serde_json::Value = serde_json::from_str(&message_json_text).unwrap();
            
            match message.message_type {
                WitMessageType::Request(_) => {
                    let action = &message_json["action"];
                    if action == "bind-app" {
                        bindings.insert(message_json["path"].as_str().unwrap().to_string(), message_json["app"].as_str().unwrap().to_string());
                    } else if action == "request" {
                        bindings::print_to_terminal("http_bindings: got request");
                        // let app = bindings.get(message_json["path"].as_str().unwrap()).unwrap();
                        match bindings.get(message_json["path"].as_str().unwrap()) {
                            Some(app) => {
                                bindings::print_to_terminal("http_bindings: properly unwrapped");
                                bindings::yield_results(vec![
                                    bindings::WitProtomessage {
                                        protomessage_type: WitProtomessageType::Request(
                                            WitRequestTypeWithTarget {
                                                is_expecting_response: true,
                                                target_ship: our.as_str(),
                                                target_app: app,
                                            }
                                        ),
                                        payload: &WitPayload {
                                            json: Some(serde_json::json!({
                                                "path": message_json["path"],
                                                "method": message_json["method"],
                                                "headers": message_json["headers"],
                                                "id": message_json["id"],
                                            }).to_string()),
                                            bytes: message.payload.bytes,
                                        },
                                    }
                                ].as_slice());
                            },
                            None => {
                                bindings::print_to_terminal("http_bindings: failed to unwrap");
                                bindings::yield_results(vec![
                                    bindings::WitProtomessage {
                                        protomessage_type: WitProtomessageType::Response,
                                        payload: &WitPayload {
                                            json: Some(serde_json::json!({
                                                "id": message_json["id"],
                                                "status": 404,
                                                "headers": {"Content-Type": "text/plain"},
                                                
                                            }).to_string()),
                                            bytes: Some("404 Not Found".as_bytes().to_vec()),
                                        },
                                    }
                                ].as_slice());
                            },
                        }
                    } else {
                        bindings::print_to_terminal(
                            format!(
                                "http_bindings: unexpected action: {:?}",
                                &message_json["action"],
                            ).as_str()
                        );
                    }
                },
                WitMessageType::Response => { // TODO might be able to get rid of this if we structure it as a Request(false)?
                    bindings::yield_results(vec![
                        bindings::WitProtomessage {
                            protomessage_type: WitProtomessageType::Response,
                            payload: &WitPayload {
                                json: Some(serde_json::json!({
                                    "id": message_json["id"],
                                    "status": message_json["status"],
                                    "headers": message_json["headers"],
                                }).to_string()),
                                bytes: message.payload.bytes,
                            },
                        }
                    ].as_slice());
                },
            }
        }
    }
}

bindings::export!(Component);
