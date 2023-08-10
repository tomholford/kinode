cargo_component_bindings::generate!();

use bindings::component::microkernel_process::types::WitPayload;
use bindings::component::microkernel_process::types::WitProcessNode;
use bindings::component::microkernel_process::types::WitProtomessageType;
use bindings::component::microkernel_process::types::WitRequestTypeWithTarget;
use std::collections::HashMap;
use serde_json::json;
struct Component;
mod process_lib;

const PROXY_HOME_PAGE: &str = include_str!("http-proxy.html");

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
                      }
                  ),
                  payload: WitPayload {
                      json: Some(serde_json::json!({
                          "action": "bind-app",
                          "path": "/apps/proxy",
                          "app": process_name
                      }).to_string()),
                      bytes: None
                  }
              },
              "".into(),
          ), (
              bindings::WitProtomessage {
                  protomessage_type: WitProtomessageType::Request(
                      WitRequestTypeWithTarget {
                          is_expecting_response: false,
                          target: WitProcessNode {
                              node: our_name.clone(),
                              process: "http_bindings".into(),
                          },
                      }
                  ),
                  payload: WitPayload {
                      json: Some(serde_json::json!({
                          "action": "bind-app",
                          "path": "/proxy/list",
                          "app": process_name
                      }).to_string()),
                      bytes: None
                  }
              },
              "".into(),
            ), (
                bindings::WitProtomessage {
                    protomessage_type: WitProtomessageType::Request(
                        WitRequestTypeWithTarget {
                            is_expecting_response: false,
                            target: WitProcessNode {
                                node: our_name.clone(),
                                process: "http_bindings".into(),
                            },
                        }
                    ),
                    payload: WitPayload {
                        json: Some(serde_json::json!({
                            "action": "bind-app",
                            "path": "/proxy/register",
                            "app": process_name
                        }).to_string()),
                        bytes: None
                    }
                },
                "".into(),
            ), (
                bindings::WitProtomessage {
                    protomessage_type: WitProtomessageType::Request(
                        WitRequestTypeWithTarget {
                            is_expecting_response: false,
                            target: WitProcessNode {
                                node: our_name.clone(),
                                process: "http_bindings".into(),
                            },
                        }
                    ),
                    payload: WitPayload {
                        json: Some(serde_json::json!({
                            "action": "bind-app",
                            "path": "/proxy/serve/:username/.*",
                            "app": process_name
                        }).to_string()),
                        bytes: None
                    }
                },
                "".into(),
            )].as_slice()
        ));

        let mut registrations: HashMap<String, String> = HashMap::new();

        loop {
            let (message, _) = bindings::await_next_message().unwrap();  //  TODO: handle error properly
            let Some(message_from_loop_string) = message.content.payload.json else {
                panic!("foo")
            };
            let message_from_loop: serde_json::Value = serde_json::from_str(&message_from_loop_string).unwrap();
            bindings::print_to_terminal(1, format!("apps-home: got request: {}", message_from_loop).as_str());
            bindings::print_to_terminal(1, format!("METHOD: {}", message_from_loop["method"]).as_str());

            if message_from_loop["path"] == "/apps/proxy" && message_from_loop["method"] == "GET" {
                bindings::yield_results(Ok(vec![(
                    bindings::WitProtomessage {
                        protomessage_type: WitProtomessageType::Response,
                        payload: WitPayload {
                            json: Some(serde_json::json!({
                                "action": "response",
                                "status": 200,
                                "headers": {
                                    "Content-Type": "text/html",
                                },
                            }).to_string()),
                            bytes: Some(PROXY_HOME_PAGE.replace("${our}", &our_name.to_string()).as_bytes().to_vec())
                        }
                    },
                    "".into(),
                )].as_slice()));
            } else if message_from_loop["path"] == "/proxy/list" && message_from_loop["method"] == "GET" {
                bindings::yield_results(Ok(vec![(
                    bindings::WitProtomessage {
                        protomessage_type: WitProtomessageType::Response,
                        payload: WitPayload {
                            json: Some(serde_json::json!({
                                "action": "response",
                                "status": 200,
                                "headers": {
                                    "Content-Type": "application/json",
                                },
                            }).to_string()),
                            bytes: Some(serde_json::json!({
                                    "registrations": registrations
                                }).to_string()
                            .as_bytes().to_vec())
                        }
                    },
                    "".into(),
                )].as_slice()));
            } else if message_from_loop["path"] == "/proxy/register" && message_from_loop["method"] == "POST" {
                let mut status = 204;
                let body_bytes = message.content.payload.bytes.unwrap_or(vec![]);
                let body_json_string = match String::from_utf8(body_bytes) {
                    Ok(s) => s,
                    Err(_) => String::new()
                };
                let body: serde_json::Value = serde_json::from_str(&body_json_string).unwrap();
                let username = body["username"].as_str().unwrap_or("");

                bindings::print_to_terminal(1, format!("Register proxy for: {}", username).as_str());

                if !username.is_empty() {
                    registrations.insert(username.to_string(), "foo".to_string());
                } else {
                    status = 400;
                }

                bindings::yield_results(Ok(vec![(
                    bindings::WitProtomessage {
                        protomessage_type: WitProtomessageType::Response,
                        payload: WitPayload {
                            json: Some(serde_json::json!({
                                "action": "response",
                                "status": status,
                                "headers": {
                                    "Content-Type": "text/html",
                                },
                            }).to_string()),
                            bytes: Some((if status == 400 { "Bad Request" } else { "Success" }).to_string().as_bytes().to_vec())
                        }
                    },
                    "".into(),
                )].as_slice()));
            } else if message_from_loop["path"] == "/proxy/register" && message_from_loop["method"] == "DELETE" {
                bindings::print_to_terminal(1, "HERE IN /proxy/register to delete something");
                let username = message_from_loop["query_params"]["username"].as_str().unwrap_or("");

                let mut status = 204;

                if !username.is_empty() {
                    registrations.remove(username);
                } else {
                    status = 400;
                }

                // TODO when we have an actual webpage, uncomment this as a response
                bindings::yield_results(Ok(vec![(
                    bindings::WitProtomessage {
                        protomessage_type: WitProtomessageType::Response,
                        payload: WitPayload {
                            json: Some(serde_json::json!({
                                "action": "response",
                                "status": status,
                                "headers": {
                                    "Content-Type": "text/html",
                                },
                            }).to_string()),
                            bytes: Some((if status == 400 { "Bad Request" } else { "Success" }).to_string().as_bytes().to_vec())
                        }
                    },
                    "".into(),
                )].as_slice()));
            } else if message_from_loop["path"] == "/proxy/serve/:username/.*" {
                let username = message_from_loop["url_params"]["username"].as_str().unwrap_or("");
                let raw_path = message_from_loop["raw_path"].as_str().unwrap_or("");
                bindings::print_to_terminal(1, format!("proxy for user: {}", username).as_str());

                if username.is_empty() || raw_path.is_empty() {
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
                                bytes: Some("Not Found".to_string().as_bytes().to_vec())
                            }
                        },
                        "".into(),
                    )].as_slice()));
                } else if !registrations.contains_key(username) {
                    bindings::yield_results(Ok(vec![(
                        bindings::WitProtomessage {
                            protomessage_type: WitProtomessageType::Response,
                            payload: WitPayload {
                                json: Some(serde_json::json!({
                                    "action": "response",
                                    "status": 403,
                                    "headers": {
                                        "Content-Type": "text/html",
                                    },
                                }).to_string()),
                                bytes: Some("Not Authorized".to_string().as_bytes().to_vec())
                            }
                        },
                        "".into(),
                    )].as_slice()));
                } else {
                    let path_parts: Vec<&str> = raw_path.split('/').collect();
                    let mut proxied_path = "/".to_string();

                    if let Some(pos) = path_parts.iter().position(|&x| x == "serve") {
                        proxied_path = path_parts[pos+2..].join("/");
                        bindings::print_to_terminal(1, format!("Path to proxy: /{}", proxied_path).as_str());
                    }

                    let res = process_lib::yield_and_await_response(
                        username.into(),
                        "http_bindings".into(),
                        Some(json!({
                            "action": "request",
                            "method": message_from_loop["method"],
                            "path": proxied_path,
                            "headers": message_from_loop["headers"],
                            "query_params": message_from_loop["query_params"],
                        })),
                        message.content.payload.bytes,
                    ).unwrap(); // TODO unwrap
                    bindings::print_to_terminal(1, "FINISHED YIELD AND AWAIT");
                    bindings::yield_results(Ok(vec![(
                        bindings::WitProtomessage {
                            protomessage_type: WitProtomessageType::Response,
                            payload: WitPayload {
                                json: res.content.payload.json,
                                bytes: res.content.payload.bytes,
                            }
                        },
                        "".into(),
                    )].as_slice()));
                }

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
