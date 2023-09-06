cargo_component_bindings::generate!();

use std::collections::HashMap;
use serde_json::json;
use serde::{Serialize, Deserialize};

use bindings::{MicrokernelProcess, print_to_terminal, receive, send};
use bindings::component::microkernel_process::types;

mod process_lib;

const PROXY_HOME_PAGE: &str = include_str!("http-proxy.html");

struct Component;

#[derive(Debug, Serialize, Deserialize)]
pub enum FileSystemAction {
    Read,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileSystemRequest {
    pub uri_string: String,
    pub action: FileSystemAction,
}

impl MicrokernelProcess for Component {
    fn run_process(our: types::ProcessAddress) {
    // fn run_process(our_name: String, process_name: String) {
        print_to_terminal(1, "http_proxy: start");
        let Some(process_name) = our.name else {
            print_to_terminal(0, "http_proxy: require our.name set");
            panic!();
        };

        let our_bindings = types::ProcessReference {
            node: our.node.clone(),
            identifier: types::ProcessIdentifier::Name("http_bindings".into()),
        };
        send(Ok(&types::OutboundMessage::Requests(vec![(
            vec![
                types::OutboundRequest {
                    is_expecting_response: false,
                    target: our_bindings.clone(),
                    payload: process_lib::make_payload(
                        Some(serde_json::json!({
                            "action": "bind-app",
                            "path": "/http-proxy",
                            "authenticated": true,
                            "app": process_name
                        })),
                        types::OutboundPayloadBytes::None,
                    ).unwrap(),  //  TODO
                },
                types::OutboundRequest {
                    is_expecting_response: false,
                    target: our_bindings.clone(),
                    payload: process_lib::make_payload(
                        Some(serde_json::json!({
                            "action": "bind-app",
                            "path": "/http-proxy/static/.*",
                            "authenticated": true,
                            "app": process_name
                        })),
                        types::OutboundPayloadBytes::None,
                    ).unwrap(),  //  TODO
                },
                types::OutboundRequest {
                    is_expecting_response: false,
                    target: our_bindings.clone(),
                    payload: process_lib::make_payload(
                        Some(serde_json::json!({
                            "action": "bind-app",
                            "path": "/http-proxy/list",
                            "app": process_name
                        })),
                        types::OutboundPayloadBytes::None,
                    ).unwrap(),  //  TODO
                },
                types::OutboundRequest {
                    is_expecting_response: false,
                    target: our_bindings.clone(),
                    payload: process_lib::make_payload(
                        Some(serde_json::json!({
                            "action": "bind-app",
                            "path": "/http-proxy/register",
                            "app": process_name
                        })),
                        types::OutboundPayloadBytes::None,
                    ).unwrap(),  //  TODO
                },
                types::OutboundRequest {
                    is_expecting_response: false,
                    target: our_bindings.clone(),
                    payload: process_lib::make_payload(
                        Some(serde_json::json!({
                            "action": "bind-app",
                            "path": "/http-proxy/serve/:username/.*",
                            "app": process_name
                        })),
                        types::OutboundPayloadBytes::None,
                    ).unwrap(),  //  TODO
                },
            ],
            "".into(),
        )])));

        let mut registrations: HashMap<String, String> = HashMap::new();

        loop {
            let (message, _) = receive().unwrap();  //  TODO: handle error properly
            let types::InboundMessage::Request(types::InboundRequest {
                is_expecting_response: _,
                payload: types::InboundPayload {
                    source: _,
                    ref json,
                    bytes,
                },
            }) = message else {
                panic!("foo")
            };

            let Some(json) = json else {
                print_to_terminal(0, "http-proxy: no json payload");
                continue;
            };
            let message_from_loop: serde_json::Value = serde_json::from_str(&json).unwrap();
            print_to_terminal(1, format!("http-proxy: got request: {}", message_from_loop).as_str());

            if message_from_loop["path"] == "/http-proxy" && message_from_loop["method"] == "GET" {
                let _ = process_lib::send_response(
                    Some(serde_json::json!({
                        "action": "response",
                        "status": 200,
                        "headers": {
                            "Content-Type": "text/html",
                        },
                    })),
                    types::OutboundPayloadBytes::Some(
                        PROXY_HOME_PAGE
                            .replace("${our}", &our.node)
                            .as_bytes()
                            .to_vec()
                    ),
                    None::<String>,
                );
            } else if message_from_loop["path"] == "/http-proxy/list" && message_from_loop["method"] == "GET" {
                let _ = process_lib::send_response(
                    Some(serde_json::json!({
                        "action": "response",
                        "status": 200,
                        "headers": {
                            "Content-Type": "application/json",
                        },
                    })),
                    types::OutboundPayloadBytes::Some(
                        serde_json::json!({"registrations": registrations})
                            .to_string()
                            .as_bytes()
                            .to_vec()
                    ),
                    None::<String>,
                );
            } else if message_from_loop["path"] == "/http-proxy/register" && message_from_loop["method"] == "POST" {
                let mut status = 204;
                let types::InboundPayloadBytes::Some(body_bytes) = bytes else {
                    continue;
                };
                let body_json_string = match String::from_utf8(body_bytes) {
                    Ok(s) => s,
                    Err(_) => String::new()
                };
                let body: serde_json::Value = serde_json::from_str(&body_json_string).unwrap();
                let username = body["username"].as_str().unwrap_or("");

                print_to_terminal(1, format!("Register proxy for: {}", username).as_str());

                if !username.is_empty() {
                    registrations.insert(username.to_string(), "foo".to_string());
                } else {
                    status = 400;
                }

                let _ = process_lib::send_response(
                    Some(serde_json::json!({
                        "action": "response",
                        "status": status,
                        "headers": {
                            "Content-Type": "text/html",
                        },
                    })),
                    types::OutboundPayloadBytes::Some(
                        (if status == 400 { "Bad Request" } else { "Success" })
                            .to_string()
                            .as_bytes()
                            .to_vec()
                    ),
                    None::<String>,
                );
            } else if message_from_loop["path"] == "/http-proxy/register" && message_from_loop["method"] == "DELETE" {
                print_to_terminal(1, "HERE IN /http-proxy/register to delete something");
                let username = message_from_loop["query_params"]["username"].as_str().unwrap_or("");

                let mut status = 204;

                if !username.is_empty() {
                    registrations.remove(username);
                } else {
                    status = 400;
                }

                let _ = process_lib::send_response(
                    Some(serde_json::json!({
                        "action": "response",
                        "status": status,
                        "headers": {
                            "Content-Type": "text/html",
                        },
                    })),
                    types::OutboundPayloadBytes::Some(
                        (if status == 400 { "Bad Request" } else { "Success" })
                            .to_string()
                            .as_bytes()
                            .to_vec()
                    ),
                    None::<String>,
                );
            } else if message_from_loop["path"] == "/http-proxy/serve/:username/.*" {
                let username = message_from_loop["url_params"]["username"].as_str().unwrap_or("");
                let raw_path = message_from_loop["raw_path"].as_str().unwrap_or("");
                print_to_terminal(1, format!("proxy for user: {}", username).as_str());

                if username.is_empty() || raw_path.is_empty() {
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
                                .to_string()
                                .as_bytes()
                                .to_vec()
                        ),
                        None::<String>,
                    );
                } else if !registrations.contains_key(username) {
                    let _ = process_lib::send_response(
                        Some(serde_json::json!({
                            "action": "response",
                            "status": 403,
                            "headers": {
                                "Content-Type": "text/html",
                            },
                        })),
                        types::OutboundPayloadBytes::Some(
                            "Not Authorized"
                                .to_string()
                                .as_bytes()
                                .to_vec()
                        ),
                        None::<String>,
                    );
                } else {
                    let path_parts: Vec<&str> = raw_path.split('/').collect();
                    let mut proxied_path = "/".to_string();

                    if let Some(pos) = path_parts.iter().position(|&x| x == "serve") {
                        proxied_path = format!("/{}", path_parts[pos+2..].join("/"));
                        print_to_terminal(1, format!("Path to proxy: {}", proxied_path).as_str());
                    }

                    let bytes = match process_lib::make_outbound_bytes_from_noncircumvented_inbound(bytes) {
                        Ok(bytes) => bytes,
                        Err(_) => {
                             print_to_terminal(0, "http_proxy unexpectedly received Circumvented inbound bytes; failing HTTP request");
                             let _ = process_lib::send_response(
                                 Some(serde_json::json!({
                                     "action": "response",
                                     "status": 404,
                                     "headers": {
                                         "Content-Type": "text/html",
                                     },
                                 })),
                                 types::OutboundPayloadBytes::Some(
                                     "http_proxy unexpectedly received Circumvented inbound bytes; failing HTTP request"
                                         .to_string()
                                         .as_bytes()
                                         .to_vec()
                                 ),
                                 None::<String>,
                             );
                             continue;
                        },
                    };

                    let res = process_lib::send_and_await_receive(
                        username.into(),
                        types::ProcessIdentifier::Name("http_bindings".into()),
                        Some(json!({
                            "action": "request",
                            "method": message_from_loop["method"],
                            "path": proxied_path,
                            "headers": message_from_loop["headers"],
                            "proxy_path": raw_path,
                            "query_params": message_from_loop["query_params"],
                        })),
                        bytes,
                    ).unwrap(); // TODO unwrap
                    let Ok(types::InboundMessage::Response(types::InboundPayload {
                        source: _,
                        json: res_json,
                        bytes: res_bytes,
                    })) = res else {
                        panic!("foobar");  //  TODO
                    };


                    print_to_terminal(1, "FINISHED YIELD AND AWAIT");
                    match res_json {
                        Some(ref json) => {
                            if json.contains("Offline") {
                                let _ = process_lib::send_response(
                                    Some(serde_json::json!({
                                        "action": "response",
                                        "status": 404,
                                        "headers": {
                                            "Content-Type": "text/html",
                                        },
                                    })),
                                    types::OutboundPayloadBytes::Some(
                                        "Node is offline"
                                            .to_string()
                                            .as_bytes()
                                            .to_vec()
                                    ),
                                    None::<String>,
                                );
                            } else {
                                let res_bytes = process_lib::make_outbound_bytes_from_noncircumvented_inbound(
                                    res_bytes,
                                ).unwrap();
                                let _ = process_lib::send_response(
                                    res_json,
                                    res_bytes,
                                    None::<String>,
                                );
                            }
                        },
                        None => {
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
                                        .to_string()
                                        .as_bytes()
                                        .to_vec()
                                ),
                                None::<String>,
                            );
                        },
                    };
                }
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
                            .to_string()
                            .as_bytes()
                            .to_vec()
                    ),
                    None::<String>,
                );
            }
        }
    }
}
