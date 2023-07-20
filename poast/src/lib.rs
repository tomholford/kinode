use serde_json::json;
use bindings::component::microkernel_process::types::*;

struct Component;

/*
    After installing this app, you can perform a GET request at /poast
    Or you can send a POST request to /poast and this app will print it out
*/

impl bindings::MicrokernelProcess for Component {
    fn init(source_ship: String, source_app: String) -> Vec<bindings::WitMessage> {
        bindings::set_state(serde_json::to_string(&json!([])).unwrap().as_str());
        vec![
            // accent GETs at /poast
            WitMessage {
                message_type: WitMessageType::Request(false),
                wire: WitWire {
                    source_ship: source_ship.clone(),
                    source_app:  source_app.clone(),
                    target_ship: source_ship.clone(),
                    target_app:  "http_server".to_string(),
                },
                payload: WitPayload {
                    json: Some(json!({
                        "SetResponse":
                            {
                                "path":"poast",
                                "content":"<h1>welcome to poast</h1>"
                            }
                        }
                    ).to_string()),
                    bytes: None
                }
            },
            // accept POSTs at /poast
            WitMessage {
                message_type: WitMessageType::Request(false),
                wire: WitWire {
                    source_ship: source_ship.clone(),
                    source_app:  source_app,
                    target_ship: source_ship.clone(),
                    target_app:  "http_server".to_string(),
                },
                payload: WitPayload {
                    json: Some(json!({
                        "Connect":
                            {
                                "path":"poast",
                                "app":"poast"
                            }
                        }
                    ).to_string()),
                    bytes: None
                }
            }
        ]
    }

    fn run_write(
        mut message_stack: Vec<bindings::WitMessage>
    ) -> Vec<(bindings::WitMessageTypeWithTarget, bindings::WitPayload)> {
        let message = message_stack.pop().unwrap();

        let Some(message_from_loop) = message.payload.json else {
            panic!("foo")
        };
        bindings::print_to_terminal(format!("Received a POST request: {}", &message_from_loop).as_str());
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
