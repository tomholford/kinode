cargo_component_bindings::generate!();

use bindings::{MicrokernelProcess, print_to_terminal, receive};
use bindings::component::microkernel_process::types;

mod process_lib;

struct Component;

const APPS_HOME_PAGE: &str = include_str!("home.html");

impl MicrokernelProcess for Component {
    fn run_process(our: types::ProcessAddress) {
        print_to_terminal(1, "apps_home: start");
        let Some(process_name) = our.name else {
            print_to_terminal(0, "apps_home: require our.name set");
            panic!();
        };

        let _ = process_lib::send_one_request(
            false,
            &our.node,
            types::ProcessIdentifier::Name("http_bindings".into()),
            Some(serde_json::json!({
                "action": "bind-app",
                "path": "/",
                "app": process_name,
                "authenticated": true,
            })),
            types::OutboundPayloadBytes::None,
            None::<String>,
        );

        loop {
            let (message, _) = receive().unwrap();  //  TODO: handle error properly
            let types::InboundMessage::Request(types::InboundRequest {
                is_expecting_response: _,
                payload: types::InboundPayload {
                    source: _,
                    ref json,
                    bytes: _,
                },
            }) = message else {
                panic!("foo")
            };
            let Some(json) = json else {
                panic!("bar");
            };
            let message_from_loop: serde_json::Value = serde_json::from_str(json).unwrap();
            print_to_terminal(1, format!("apps-home: got request: {}", message_from_loop).as_str());
            print_to_terminal(1, format!("METHOD: {}", message_from_loop["method"]).as_str());

            if message_from_loop["path"] == "/" && message_from_loop["method"] == "GET" {
                let _ = process_lib::send_response(
                    Some(serde_json::json!({
                        "action": "response",
                        "status": 200,
                        "headers": {
                            "Content-Type": "text/html",
                        },
                    })),
                    types::OutboundPayloadBytes::Some(
                        APPS_HOME_PAGE
                            .replace("${our}", &our.node.to_string())
                            .as_bytes()
                            .to_vec()
                    ),
                    None::<String>,
                );
            } else {
                let _ = process_lib::send_response(
                    Some(serde_json::json!({
                        "action": "response",
                        "status": 404,
                        "headers": {
                            "Content-Type": "text/html",
                        },
                    })),
                    types::OutboundPayloadBytes::Some(
                        "Not Found"
                            .as_bytes()
                            .to_vec()
                    ),
                    None::<String>,
                );
            }
        }
    }
}
