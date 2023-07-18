use serde_json::json;
use bindings::component::microkernel_process::types::*;

struct Component;

impl bindings::MicrokernelProcess for Component {
    fn init(source_ship: String, source_app: String) -> Vec<bindings::WitMessage> {
        bindings::set_state(serde_json::to_string(&json!([])).unwrap().as_str());
        vec![
            WitMessage {
                message_type: WitMessageType::Response, // TODO no lol
                wire: WitWire {
                    source_ship: source_ship.clone(),
                    source_app:  source_app,
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
        let mut response_string = "\"".to_string();
        response_string.push_str(&message_from_loop);
        response_string.push_str(" appended by poast\"");
        let response = bindings::component::microkernel_process::types::WitPayload {
            json: Some(response_string.clone()),
            bytes: None,
        };
        let state_string = bindings::fetch_state("");
        let mut state = serde_json::from_str(&state_string).unwrap();
        state = match state {
            serde_json::Value::Array(mut vector) => {
                vector.push(serde_json::to_value(response_string.clone()).unwrap());
                serde_json::Value::Array(vector)
            },
            _ => json!([response_string.clone()])  // TODO
        };
        bindings::set_state(serde_json::to_string(&state).unwrap().as_str());
        // bindings::to_event_loop(
        //     &message.wire.source_ship.clone(),
        //     &"http_server".to_string(),
        //     bindings::WitMessageType::Request(false),
        //     &response
        // );
        vec![(
            bindings::WitMessageTypeWithTarget::Request(
                WitRequestTypeWithTarget {
                    is_expecting_response: false,
                    target_ship: message.wire.source_ship.clone(),
                    target_app: "http_server".to_string(),
                }
            ),
            response,
        )]
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
