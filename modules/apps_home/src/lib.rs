cargo_component_bindings::generate!();

use bindings::component::microkernel_process::types;
struct Component;

const APPS_HOME_PAGE: &str = include_str!("home.html");

impl bindings::MicrokernelProcess for Component {
    fn run_process(our: types::WitProcessAddress) {
    // fn run_process(our_name: String, process_name: String) {
        bindings::print_to_terminal(1, "apps_home: start");
        let Some(process_name) = our.name else {
            bindings::print_to_terminal(0, "apps_home: require our.name set");
            panic!();
        };
        bindings::send_requests(Ok((
            vec![
                types::WitProtorequest {
                    is_expecting_response: false,
                    target: types::WitProcessReference {
                        node: our.node.clone(),
                        identifier: types::WitProcessIdentifier::Name("http_bindings".into()),
                    },
                    payload: types::WitPayload {
                        json: Some(serde_json::json!({
                            "action": "bind-app",
                            "path": "/",
                            "app": process_name,
                            "authenticated": true
                        }).to_string()),
                        bytes: types::WitPayloadBytes {
                            circumvent: types::WitCircumvent::False,
                            content: None,
                        },
                    },
                },
            ].as_slice(),
            "".into(),
        )));

        loop {
            let (message, _) = bindings::await_next_message().unwrap();  //  TODO: handle error properly
            let Some(message_from_loop_string) = message.content.payload.json else {
                panic!("foo")
            };
            let message_from_loop: serde_json::Value = serde_json::from_str(&message_from_loop_string).unwrap();
            bindings::print_to_terminal(1, format!("apps-home: got request: {}", message_from_loop).as_str());
            bindings::print_to_terminal(1, format!("METHOD: {}", message_from_loop["method"]).as_str());

            if message_from_loop["path"] == "/" && message_from_loop["method"] == "GET" {
                bindings::send_response(Ok((
                    &types::WitPayload {
                        json: Some(serde_json::json!({
                            "action": "response",
                            "status": 200,
                            "headers": {
                                "Content-Type": "text/html",
                            },
                        }).to_string()),
                        bytes: types::WitPayloadBytes {
                            circumvent: types::WitCircumvent::False,
                            content: Some(APPS_HOME_PAGE.replace("${our}", &our.node.to_string()).as_bytes().to_vec()),
                        },
                    },
                    "".into(),
                )));
            } else {
                bindings::send_response(Ok((
                    &types::WitPayload {
                        json: Some(serde_json::json!({
                            "action": "response",
                            "status": 404,
                            "headers": {
                                "Content-Type": "text/html",
                            },
                        }).to_string()),
                        bytes: types::WitPayloadBytes {
                            circumvent: types::WitCircumvent::False,
                            content: Some("Not Found".as_bytes().to_vec()),
                        },
                    },
                    "".into(),
                )));
            }
        }
    }
}
