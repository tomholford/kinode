use crate::types::*;
use http::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize)]
struct HttpClientRequest {
    uri: String,
    method: String,
    headers: HashMap<String, String>,
    body: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct HttpClientResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
}

// Test http_client with these commands in the terminal
// !message tuna http_client {"method": "GET", "uri": "https://jsonplaceholder.typicode.com/posts", "headers": {}, "body": ""}
// !message tuna http_client {"method": "POST", "uri": "https://jsonplaceholder.typicode.com/posts", "headers": {"Content-Type": "application/json"}, "body": "{\"title\": \"foo\", \"body\": \"bar\"}"}
// !message tuna http_client {"method": "PUT", "uri": "https://jsonplaceholder.typicode.com/posts", "headers": {"Content-Type": "application/json"}, "body": "{\"title\": \"foo\", \"body\": \"bar\"}"}

pub async fn http_client(
    our_name: String,
    send_to_loop: MessageSender,
    mut recv_in_client: MessageReceiver,
    print_tx: PrintSender,
) {
    while let Some(message) = recv_in_client.recv().await {
        tokio::spawn(handle_message(
            our_name.clone(),
            send_to_loop.clone(),
            message,
            print_tx.clone(),
        ));
    }
}

async fn handle_message(
    our: String,
    send_to_loop: MessageSender,
    wm: WrappedMessage,
    _print_tx: PrintSender,
) {
    let WrappedMessage { ref id, target: _, ref rsvp, message: Ok(Message { ref source, ref content }), }
            = wm else {
        panic!("filesystem: unexpected Error")  //  TODO: implement error handling
    };

    let target = match content.message_type {
        MessageType::Response => panic!("http_client: should not get a response message"),
        MessageType::Request(is_expecting_response) => {
            if is_expecting_response {
                ProcessNode {
                    node: our.clone(),
                    process: source.process.clone(),
                }
            } else {
                let Some(rsvp) = rsvp else { panic!("http_client: no rsvp"); };
                rsvp.clone()
            }
        }
    };

    let Some(value) = content.payload.json.clone() else {
        panic!("http_client: request must have JSON payload, got: {:?}", wm);
    };

    let req: HttpClientRequest = match serde_json::from_value(value) {
        Ok(req) => req,
        Err(e) => panic!("http_client: failed to parse request: {:?}", e),
    };

    let client = reqwest::Client::new();

    let request_builder = match req.method.to_uppercase()[..].to_string().as_str() {
        "GET" => client.get(req.uri),
        "PUT" => client.put(req.uri),
        "POST" => client.post(req.uri),
        "DELETE" => client.delete(req.uri),
        _ => panic!("Unsupported HTTP method: {}", req.method),
    };

    let request = request_builder
        .headers(deserialize_headers(req.headers))
        .body(req.body.clone())
        .build()
        .unwrap();

    let response = match client.execute(request).await {
        Ok(response) => response,
        Err(e) => panic!("http_client: failed to execute request: {:?}", e),
    };

    let http_client_response = HttpClientResponse {
        status: response.status().as_u16(),
        headers: serialize_headers(&response.headers().clone()),
    };

    let message = WrappedMessage {
        id: id.clone(),
        target,
        rsvp: None,
        message: Ok(Message {
            source: ProcessNode {
                node: our.clone(),
                process: "http_client".into(),
            },
            content: MessageContent {
                message_type: MessageType::Response,
                payload: Payload {
                    json: Some(serde_json::to_value(http_client_response).unwrap()),
                    bytes: Some(response.bytes().await.unwrap().to_vec()),
                },
            },
        }),
    };

    send_to_loop.send(message).await.unwrap();
}

//
//  helpers
//
fn serialize_headers(headers: &HeaderMap) -> HashMap<String, String> {
    let mut hashmap = HashMap::new();
    for (key, value) in headers.iter() {
        let key_str = key.to_string();
        let value_str = value.to_str().unwrap_or("").to_string();
        hashmap.insert(key_str, value_str);
    }
    hashmap
}

fn deserialize_headers(hashmap: HashMap<String, String>) -> HeaderMap {
    let mut header_map = HeaderMap::new();
    for (key, value) in hashmap {
        let key_bytes = key.as_bytes();
        let key_name = HeaderName::from_bytes(key_bytes).unwrap();
        let value_header = HeaderValue::from_str(&value).unwrap();
        header_map.insert(key_name, value_header);
    }
    header_map
}
