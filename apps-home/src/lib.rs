use bindings::component::microkernel_process::types::WitProtomessageType;
use bindings::component::microkernel_process::types::WitRequestTypeWithTarget;
use bindings::component::microkernel_process::types::WitPayload;
struct Component;

const APPS_HOME_PAGE: &str = include_str!("home.html");

impl bindings::MicrokernelProcess for Component {
    fn run_process(our: String, dap: String) {
        bindings::print_to_terminal("apps-home: start");
        bindings::yield_results(
          vec![(
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
                          "path": "/",
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
            bindings::print_to_terminal(format!("apps-home: got request: {}", message_from_loop).as_str());
            bindings::print_to_terminal(format!("METHOD: {}", message_from_loop["method"]).as_str());

            if message_from_loop["path"] == "/" && message_from_loop["method"] == "GET" {
                bindings::yield_results(vec![(
                    bindings::WitProtomessage {
                        protomessage_type: WitProtomessageType::Response,
                        payload: &WitPayload {
                            json: Some(serde_json::json!({
                                "action": "response",
                                "status": 200,
                                "headers": {
                                    "Content-Type": "text/html",
                                },
                            }).to_string()),
                            bytes: Some(APPS_HOME_PAGE.replace("${our}", &our.to_string()).as_bytes().to_vec())
                        }
                    },
                    "",
                )].as_slice());
            } else {
                bindings::yield_results(vec![(
                    bindings::WitProtomessage {
                        protomessage_type: WitProtomessageType::Response,
                        payload: &WitPayload {
                            json: Some(serde_json::json!({
                                "action": "response",
                                "status": 404,
                                "headers": {
                                    "Content-Type": "text/html",
                                },
                            }).to_string()),
                            bytes: Some("Not Found".as_bytes().to_vec())
                        }
                    },
                    "",
                )].as_slice());
            }
        }
    }
}

bindings::export!(Component);
