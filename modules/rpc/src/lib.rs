cargo_component_bindings::generate!();

use bindings::{print_to_terminal, receive, send_requests, send_and_await_response, send_response, get_payload, Guest};
use bindings::component::uq_process::types::*;
use serde_json::json;
use serde::{Serialize, Deserialize};
use std::collections::HashMap;
extern crate base64;

mod process_lib;

struct Component;

#[derive(Debug, Serialize, Deserialize)]
struct RpcMessage {
    pub node: String,
    pub process: String,
    pub inherit: Option<bool>,
    pub expects_response: Option<bool>, // always false?
    pub ipc: Option<String>,
    pub metadata: Option<String>,
    pub context: Option<String>,
    pub mime: Option<String>,
    pub data: Option<String>,
}

fn send_http_response(
    status: u16,
    headers: HashMap<String, String>,
    payload_bytes: Vec<u8>,
) {
    send_response(
        &Response {
            ipc: Some(serde_json::json!({
                "status": status,
                "headers": headers,
            }).to_string()),
            metadata: None,
        },
        Some(&Payload {
            mime: Some("application/octet-stream".to_string()),
            bytes: payload_bytes,
        })
    )
}

const RPC_PAGE: &str = include_str!("rpc.html");

fn binary_encoded_string_to_bytes(s: &str) -> Vec<u8> {
    s.chars().map(|c| c as u8).collect()
}

impl Guest for Component {
    fn init(our: Address) {
        print_to_terminal(0, "RPC: start");

        let bindings_address = Address {
            node: our.node.clone(),
            process: ProcessId::Name("http_bindings".to_string()),
        };

        // <address, request, option<context>, option<payload>>
        let http_endpoint_binding_requests: [(Address, Request, Option<Context>, Option<Payload>); 1] = [
            // (
            //     bindings_address.clone(),
            //     Request {
            //         inherit: false,
            //         expects_response: None,
            //         ipc: Some(serde_json::json!({
            //             "action": "bind-app",
            //             "path": "/rpc",
            //             "app": "rpc",
            //             "local_only": true,
            //         }).to_string()),
            //         metadata: None,
            //     },
            //     None,
            //     None
            // ),
            (
                bindings_address.clone(),
                Request {
                    inherit: false,
                    expects_response: None,
                    ipc: Some(serde_json::json!({
                        "action": "bind-app",
                        "path": "/rpc/message",
                        "app": "rpc",
                        "local_only": true,
                    }).to_string()),
                    metadata: None,
                },
                None,
                None
            ),
        ];
        send_requests(&http_endpoint_binding_requests);

        loop {
            let Ok((_source, message)) = receive() else {
                print_to_terminal(0, "rpc: got network error");
                continue;
            };
            let Message::Request(request) = message else {
                print_to_terminal(0, "rpc: got unexpected message");
                continue;
            };

            if let Some(json) = request.ipc {
                print_to_terminal(1, format!("rpc: JSON {}", json).as_str());
                let message_json: serde_json::Value = match serde_json::from_str(&json) {
                    Ok(v) => v,
                    Err(_) => {
                        print_to_terminal(1, "rpc: failed to parse ipc JSON, skipping");
                        continue;
                    },
                };

                print_to_terminal(1, "rpc: parsed ipc JSON");

                let path = message_json["path"].as_str().unwrap_or("");

                let mut default_headers = HashMap::new();
                default_headers.insert("Content-Type".to_string(), "text/html".to_string());
                // Handle incoming http
                match path {
                    "/rpc" => {
                        if message_json["method"] == "GET" {
                            send_response(
                                &Response {
                                    ipc: Some(serde_json::json!({
                                        "action": "response",
                                        "status": 200,
                                        "headers": {
                                            "Content-Type": "text/html",
                                        },
                                    }).to_string()),
                                    metadata: None,
                                },
                                Some(&Payload {
                                    mime: Some("text/html".to_string()),
                                    bytes: RPC_PAGE.replace("${our}", &our.node).to_string().as_bytes().to_vec(),
                                }),
                            );
                        }
                    }
                    "/rpc/message" => {
                        let Some(payload) = get_payload() else {
                            print_to_terminal(1, "rpc: no bytes in payload, skipping...");
                            send_http_response(400, default_headers.clone(), "No payload".to_string().as_bytes().to_vec());
                            continue;
                        };
                        let body_json: RpcMessage = match serde_json::from_slice::<RpcMessage>(&payload.bytes) {
                            Ok(v) => v,
                            Err(_) => {
                                print_to_terminal(1, &format!("rpc: JSON is not valid RpcMessage: {:?}", serde_json::from_slice::<serde_json::Value>(&payload.bytes)));
                                send_http_response(400, default_headers.clone(), "JSON is not valid RpcMessage".to_string().as_bytes().to_vec());
                                continue;
                            },
                        };

                        let payload = match base64::decode(&body_json.data.unwrap_or("".to_string())) {
                            Ok(bytes) => Some(Payload {
                                mime: body_json.mime,
                                bytes,
                            }),
                            Err(_) => None,
                        };

                        let result = send_and_await_response(
                            &Address {
                                node: body_json.node,
                                process: ProcessId::Name(body_json.process),
                            },
                            &Request {
                                inherit: false,
                                expects_response: Some(5), // TODO evaluate timeout
                                ipc: body_json.ipc,
                                metadata: body_json.metadata,
                            },
                            payload.as_ref(),
                        );

                        match result {
                            Ok((_source, message)) => {
                                let Message::Response((response, _context)) = message else {
                                    print_to_terminal(1, "rpc: got unexpected response to message");
                                    send_http_response(500, default_headers, "Invalid Internal Response".to_string().as_bytes().to_vec());
                                    continue;
                                };

                                let (mime, data) = match get_payload() {
                                    Some(p) => {
                                        let mime = match p.mime {
                                            Some(mime) => mime,
                                            None => "application/octet-stream".to_string(),
                                        };
                                        let bytes = p.bytes;

                                        (mime, base64::encode(bytes))
                                    },
                                    None => ("".to_string(), "".to_string()),
                                };

                                let body = serde_json::json!({
                                    "ipc": response.ipc,
                                    "payload": {
                                        "mime": mime,
                                        "data": data,
                                    },
                                }).to_string().as_bytes().to_vec();

                                send_http_response(200, default_headers.clone(), body);
                                continue;
                            }
                            Err(error) => {
                                print_to_terminal(1, "rpc: error coming back");
                                send_http_response(500, default_headers.clone(), "Network Error".to_string().as_bytes().to_vec());
                                continue;
                            }
                        }
                    }
                    _ => {
                        send_http_response(404, default_headers.clone(), "Not Found".to_string().as_bytes().to_vec());
                        continue;
                    }
                }
            }
        }
    }
}
