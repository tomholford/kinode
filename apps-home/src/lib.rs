cargo_component_bindings::generate!();

use bindings::component::microkernel_process::types::WitPayload;
use bindings::component::microkernel_process::types::WitProcessNode;
use bindings::component::microkernel_process::types::WitProtomessageType;
use bindings::component::microkernel_process::types::WitRequestTypeWithTarget;
struct Component;

const APPS_HOME_PAGE: &str = include_str!("home.html");

impl bindings::MicrokernelProcess for Component {
    fn run_process(our_name: String, process_name: String) {
        bindings::print_to_terminal(1, "apps-home: start");
        bindings::yield_results(Ok(
          vec![(
              bindings::WitProtomessage {
                  protomessage_type: WitProtomessageType::Request(
                      WitRequestTypeWithTarget {
                          is_expecting_response: false,
                          target: WitProcessNode {
                              node: our_name.clone(),
                              process: "http_bindings".into(),
                          },
                          // target_ship: our.as_str(),
                          // target_app: "http_bindings",
                      }
                  ),
                  payload: WitPayload {
                      json: Some(serde_json::json!({
                          "action": "bind-app",
                          "path": "/",
                          "app": process_name, 
                      }).to_string()),
                      bytes: None
                  }
              },
              "".into(),
            )].as_slice()
        ));

        loop {
            let (message, _) = bindings::await_next_message().unwrap();  //  TODO: handle error properly
            let Some(message_from_loop_string) = message.content.payload.json else {
                panic!("foo")
            };
            let message_from_loop: serde_json::Value = serde_json::from_str(&message_from_loop_string).unwrap();
            bindings::print_to_terminal(1, format!("apps-home: got request: {}", message_from_loop).as_str());
            bindings::print_to_terminal(1, format!("METHOD: {}", message_from_loop["method"]).as_str());

            if message_from_loop["path"] == "/" && message_from_loop["method"] == "GET" {
                bindings::yield_results(Ok(vec![(
                    bindings::WitProtomessage {
                        protomessage_type: WitProtomessageType::Response,
                        payload: WitPayload {
                            json: Some(serde_json::json!({
                                "action": "response",
                                "status": 200,
                                "headers": {
                                    "Content-Type": "text/html",
                                    "Access-Control-Allow-Origin": "*"
                                },
                            }).to_string()),
                            bytes: Some(APPS_HOME_PAGE.replace("${our}", &our_name.to_string()).as_bytes().to_vec())
                        }
                    },
                    "".into(),
                )].as_slice()));
            } else {
                bindings::yield_results(Ok(vec![(
                    bindings::WitProtomessage {
                        protomessage_type: WitProtomessageType::Response,
                        payload: WitPayload {
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
                    "".into(),
                )].as_slice()));
            }
        }
    }
}
