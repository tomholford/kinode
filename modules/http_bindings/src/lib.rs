cargo_component_bindings::generate!();

use std::collections::HashMap;
use serde::{Serialize, Deserialize};
use hmac::{Hmac, Mac};
use jwt::{SignWithKey, VerifyWithKey, Error};
use sha2::Sha256;
use url::form_urlencoded;

use bindings::{MicrokernelProcess, print_to_terminal, receive};
use bindings::component::microkernel_process::types;

mod process_lib;

struct Component;

struct BoundPath {
    app: String,
    authenticated: bool,
}

#[derive(Serialize, Deserialize)]
struct JwtClaims {
  username: String,
  expiration: u64,
}

fn generate_token(our_name: String, secret: Hmac<Sha256>) -> Option<String> {
    let claims = JwtClaims {
        username: our_name,
        expiration: 0,
    };
    let token: Option<String> = match claims.sign_with_key(&secret) {
        Ok(token) => Some(token),
        Err(_) => None,
    };
    token
}

fn auth_cookie_valid(our_name: String, cookie: &str, secret: Hmac<Sha256>) -> bool {
    let cookie_parts: Vec<&str> = cookie.split("; ").collect();
    let mut auth_token = None;
    for cookie_part in cookie_parts {
        let cookie_part_parts: Vec<&str> = cookie_part.split("=").collect();
        if cookie_part_parts.len() == 2 && cookie_part_parts[0] == format!("uqbar-auth_{}", our_name) {
            auth_token = Some(cookie_part_parts[1].to_string());
            break;
        }
    }

    let auth_token = match auth_token {
        Some(token) if !token.is_empty() => token,
        _ => return false,
    };

    print_to_terminal(1, format!("http_bindings: auth_token: {}", auth_token).as_str());

    let claims: Result<JwtClaims, Error> = auth_token.verify_with_key(&secret);

    match claims {
        Ok(data) => {
            print_to_terminal(1, format!("http_bindings: our name: {}, token_name {}", our_name, data.username).as_str());
            data.username == our_name
        },
        Err(_) => {
            print_to_terminal(1, "http_bindings: failed to verify token");
            false
        },
    }
}

fn send_http_response(
    id: String,
    status: u16,
    headers: HashMap<String, String>,
    payload_bytes: Vec<u8>,
) {
    let _ = process_lib::send_response(
        Some(serde_json::json!({
            "id": id,
            "status": status,
            "headers": headers,
        })),
        types::OutboundPayloadBytes::Some(payload_bytes),
        None::<String>,
    );
}

// TODO: handle auth correctly, generate a secret and store in filesystem if non-existent
impl MicrokernelProcess for Component {
    fn run_process(our: types::ProcessAddress) {
        print_to_terminal(1, "http_bindings: start");
        let mut path_bindings: HashMap<String, BoundPath> = HashMap::new();
        let mut jwt_secret: Option<Hmac<Sha256>> = None;

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
                print_to_terminal(1, "http_bindings: got unexpected response");
                continue;
            };

            let Some(json) = json else {
                panic!("bar");
            };
            let message_json: serde_json::Value = match serde_json::from_str(json) {
                Ok(v) => v,
                Err(_) => {
                    print_to_terminal(1, "http_bindings: failed to parse message_json_text");
                    continue;
                },
            };

            print_to_terminal(1, "http_bindings: GOT MESSAGE");

            let action = &message_json["action"];
            // Safely unwrap the path as a string
            let path = match message_json["path"].as_str() {
                Some(s) => s,
                None => "", // or any other default value
            };
            let app = match message_json["app"].as_str() {
                Some(s) => s,
                None => "", // or any other default value
            };

            if action == "set-jwt-secret" {
                let types::InboundPayloadBytes::Some(jwt_secret_bytes) = bytes else {
                    panic!("set-jwt-secrect with no bytes");
                };

                if jwt_secret_bytes.is_empty() {
                    print_to_terminal(1, "http_bindings: got empty jwt_secret_bytes");
                } else {
                    print_to_terminal(1, "http_bindings: generating token secret...");
                    jwt_secret = match Hmac::new_from_slice(&jwt_secret_bytes) {
                        Ok(secret) => Some(secret),
                        Err(_) => {
                            print_to_terminal(1, "http_bindings: failed to generate token secret");
                            None
                        },
                    };
                }
                let _ = process_lib::send_response(
                    None::<String>,
                    types::OutboundPayloadBytes::None,
                    None::<String>,
                );
            } else if action == "bind-app" && path != "" && app != "" {
                let path_segments = path.trim_start_matches('/').split("/").collect::<Vec<&str>>();
                if app != "apps_home" && (path_segments.is_empty() || path_segments[0] != app.clone().replace("_", "-")) {
                    print_to_terminal(1, "http_bindings: first path segment does not match process");
                    continue;
                } else {
                    path_bindings.insert(path.to_string(), {
                        BoundPath {
                            app: app.to_string(),
                            authenticated: message_json.get("authenticated").and_then(|v| v.as_bool()).unwrap_or(false),
                        }
                    });
                }
            } else if action == "request" {
                print_to_terminal(1, "http_bindings: got request");

                // Start Login logic
                if path == "/login" {
                    print_to_terminal(1, "http_bindings: got login request");

                    if message_json["method"] == "GET" {
                        print_to_terminal(1, "http_bindings: got login GET request");
                        let login_page_content = include_str!("login.html");
                        let personalized_login_page = login_page_content.replace("${our}", &our.node);

                        send_http_response(message_json["id"].to_string(), 200, {
                            let mut headers = HashMap::new();
                            headers.insert("Content-Type".to_string(), "text/html".to_string());
                            headers
                        }, personalized_login_page.as_bytes().to_vec());
                    } else if message_json["method"] == "POST" {
                        print_to_terminal(1, "http_bindings: got login POST request");

                        let types::InboundPayloadBytes::Some(body_bytes) = bytes else {
                            panic!("set-jwt-secrect with no bytes");
                        };
                        let body_json_string = match String::from_utf8(body_bytes) {
                            Ok(s) => s,
                            Err(_) => String::new()
                        };
                        let body: serde_json::Value = serde_json::from_str(&body_json_string).unwrap();
                        let password = body["password"].as_str().unwrap_or("");

                        if password == "" {
                            send_http_response(message_json["id"].to_string(), 400, HashMap::new(), "Bad Request".as_bytes().to_vec());
                        } else {
                            print_to_terminal(1, "http_bindings: generating token...");
                            // TODO: check the password

                            match jwt_secret.clone() {
                                Some(secret) => {
                                    match generate_token(our.node.clone(), secret) {
                                        Some(token) => {
                                            // Token was generated successfully; you can use it here.
                                            send_http_response(message_json["id"].to_string(), 200, {
                                                let mut headers = HashMap::new();
                                                headers.insert("Content-Type".to_string(), "text/html".to_string());
                                                headers.insert("set-cookie".to_string(), format!("uqbar-auth_{}={};", our.node, token));
                                                headers
                                            }, "".as_bytes().to_vec());
                                        }
                                        None => {
                                            send_http_response(message_json["id"].to_string(), 500, HashMap::new(), "Server Error".as_bytes().to_vec());
                                        }
                                    }
                                }
                                None => {
                                    send_http_response(message_json["id"].to_string(), 500, HashMap::new(), "Server Error".as_bytes().to_vec());
                                },
                            }
                        }
                    } else if message_json["method"] == "PUT" {
                        print_to_terminal(1, "http_bindings: got login PUT request");

                        let types::InboundPayloadBytes::Some(body_bytes) = bytes else {
                            panic!("set-jwt-secrect with no bytes");
                        };
                        let body_json_string = match String::from_utf8(body_bytes) {
                            Ok(s) => s,
                            Err(_) => String::new()
                        };
                        let body: serde_json::Value = serde_json::from_str(&body_json_string).unwrap();
                        // let password = body["password"].as_str().unwrap_or("");
                        let signature = body["signature"].as_str().unwrap_or("");

                        if signature == "" {
                            send_http_response(message_json["id"].to_string(), 400, HashMap::new(), "Bad Request".as_bytes().to_vec());
                        } else {
                            // TODO: Check signature against our address
                            print_to_terminal(1, "http_bindings: generating secret...");
                            // jwt_secret = generate_secret(password);
                            print_to_terminal(1, "http_bindings: generating token...");

                            match jwt_secret.clone() {
                                Some(secret) => {
                                    match generate_token(our.node.clone(), secret) {
                                        Some(token) => {
                                            // Token was generated successfully; you can use it here.
                                            send_http_response(message_json["id"].to_string(), 200, {
                                                let mut headers = HashMap::new();
                                                headers.insert("Content-Type".to_string(), "text/html".to_string());
                                                headers.insert("set-cookie".to_string(), format!("uqbar-auth_{}={};", our.node, token));
                                                headers
                                            }, "".as_bytes().to_vec());
                                        }
                                        None => {
                                            // Failed to generate token; you should probably return an error.
                                            send_http_response(message_json["id"].to_string(), 500, HashMap::new(), "Server Error".as_bytes().to_vec());
                                        }
                                    }
                                }
                                None => {
                                    send_http_response(message_json["id"].to_string(), 500, HashMap::new(), "Server Error".as_bytes().to_vec());
                                }
                            }
                        }
                    } else {
                        send_http_response(message_json["id"].to_string(), 404, HashMap::new(), "Not Found".as_bytes().to_vec());
                    }
                    continue;
                }
                // End Login logic

                let path_segments = path.trim_start_matches('/').split("/").collect::<Vec<&str>>();
                let mut registered_path = path;
                let mut url_params: HashMap<String, String> = HashMap::new();

                for (key, _value) in &path_bindings {
                    let key_segments = key.trim_start_matches('/').split("/").collect::<Vec<&str>>();
                    if key_segments.len() != path_segments.len() && (!key.contains("/.*") || (key_segments.len() - 1) > path_segments.len()) {
                        continue;
                    }

                    let mut paths_match = true;
                    for i in 0..key_segments.len() {
                        if key_segments[i] == ".*" {
                            break;
                        } else if !(key_segments[i].starts_with(":") || key_segments[i] == path_segments[i]) {
                            paths_match = false;
                            break;
                        } else if key_segments[i].starts_with(":") {
                            url_params.insert(key_segments[i][1..].to_string(), path_segments[i].to_string());
                        }
                    }

                    if paths_match {
                        registered_path = key;
                        break;
                    }
                }

                print_to_terminal(1, &("http_bindings: registered path ".to_string() + registered_path));

                match path_bindings.get(registered_path) {
                    Some(bound_path) => {
                        let app = bound_path.app.as_str();
                        print_to_terminal(1, &("http_bindings: properly unwrapped path ".to_string() + registered_path));

                        if bound_path.authenticated {
                            print_to_terminal(1, "AUTHENTICATED ROUTE");
                            let auth_success = match jwt_secret.clone() {
                                Some(secret) => {
                                    print_to_terminal(1, "HAVE SECRET");
                                    auth_cookie_valid(our.node.clone(), message_json["headers"]["cookie"].as_str().unwrap_or(""), secret)
                                },
                                None => {
                                    print_to_terminal(1, "NO SECRET");
                                    false
                                }
                            };

                            if !auth_success {
                                print_to_terminal(1, "http_bindings: path");
                                let proxy_path = message_json["proxy_path"].as_str();

                                let redirect_path: String = match proxy_path {
                                    Some(pp) => form_urlencoded::byte_serialize(pp.as_bytes()).collect(),
                                    None => form_urlencoded::byte_serialize(path.as_bytes()).collect()
                                };

                                let location = match proxy_path {
                                    Some(_) => format!("/http-proxy/serve/{}/login?redirect={}", &our.node, redirect_path),
                                    None => format!("/login?redirect={}", redirect_path)
                                };

                                send_http_response(message_json["id"].to_string(), 302, {
                                    let mut headers = HashMap::new();
                                    headers.insert("Content-Type".to_string(), "text/html".to_string());
                                    headers.insert("Location".to_string(), location);
                                    headers
                                }, "Auth cookie not valid".as_bytes().to_vec());
                                continue;
                            }
                        }

                        let types::InboundPayloadBytes::Some(bytes) = bytes else {
                            panic!("set-jwt-secrect with no bytes");
                        };
                        let _ = process_lib::send_one_request(
                            false,
                            &our.node,
                            types::ProcessIdentifier::Name(app.into()),
                            Some(serde_json::json!({
                                "path": registered_path,
                                "raw_path": path,
                                "method": message_json["method"],
                                "headers": message_json["headers"],
                                "query_params": message_json["query_params"],
                                "url_params": url_params,
                                "id": message_json["id"],
                            })),
                            types::OutboundPayloadBytes::Some(bytes),
                            None::<String>,
                        );
                    },
                    None => {
                        print_to_terminal(1, "http_bindings: no app found at this path");
                        send_http_response(message_json["id"].to_string(), 404, HashMap::new(), "Not Found".as_bytes().to_vec());
                    },
                }
            } else {
                print_to_terminal(1,
                    format!(
                        "http_bindings: unexpected action: {:?}",
                        &message_json["action"],
                    ).as_str()
                );
            }
        }
    }
}
