use crate::types::*;
use anyhow::Result;
use http::header::{HeaderMap, HeaderName, HeaderValue};
use std::collections::HashMap;

// Test http_client with these commands in the terminal
// !message tuna http_client {"method": "GET", "uri": "https://jsonplaceholder.typicode.com/posts", "headers": {}, "body": ""}
// !message tuna http_client {"method": "POST", "uri": "https://jsonplaceholder.typicode.com/posts", "headers": {"Content-Type": "application/json"}, "body": "{\"title\": \"foo\", \"body\": \"bar\"}"}
// !message tuna http_client {"method": "PUT", "uri": "https://jsonplaceholder.typicode.com/posts", "headers": {"Content-Type": "application/json"}, "body": "{\"title\": \"foo\", \"body\": \"bar\"}"}

pub async fn http_client(
    our_name: String,
    send_to_loop: MessageSender,
    mut recv_in_client: MessageReceiver,
    print_tx: PrintSender,
) -> Result<()> {
    while let Some(message) = recv_in_client.recv().await {
        let KernelMessage {
            id,
            source,
            target: _,
            rsvp,
            message: Message::Request(Request {
                inherit: _,
                expects_response: is_expecting_response,
                ipc: json,
                metadata: _,
            }),
            payload: _,
        } = message.clone() else {
            return Err(anyhow::anyhow!("http_client: bad message"))
        };

        let our_name = our_name.clone();
        let send_to_loop = send_to_loop.clone();
        let print_tx = print_tx.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_message(
                our_name.clone(),
                send_to_loop.clone(),
                id,
                rsvp,
                is_expecting_response,
                source.clone(),
                json,
                print_tx.clone(),
            ).await {
                send_to_loop.send(
                    make_error_message(
                        our_name.clone(),
                        id,
                        source,
                        e,
                    )
                ).await.unwrap();
            }
        });
    }
    Err(anyhow::anyhow!("http_client: exited"))
}

async fn handle_message(
    our: String,
    send_to_loop: MessageSender,
    id: u64,
    rsvp: Option<Address>,
    expects_response: bool,
    source: Address,
    json: Option<String>,
    _print_tx: PrintSender,
) -> Result<(), HttpClientError> {
    let target =
        if expects_response {
            source.clone()
        } else {
            let Some(rsvp) = rsvp else {
                return Err(HttpClientError::BadRsvp);
            };
            rsvp.clone()
        };

    let Some(ref json) = json else {
        return Err(HttpClientError::NoJson);
    };

    let req: HttpClientRequest = match serde_json::from_str(json) {
        Ok(req) => req,
        Err(e) => return Err(HttpClientError::BadJson {
            json: json.to_string(),
            error: format!("{}", e) }
        ),
    };

    let client = reqwest::Client::new();

    let request_builder = match req.method.to_uppercase()[..].to_string().as_str() {
        "GET" => client.get(req.uri),
        "PUT" => client.put(req.uri),
        "POST" => client.post(req.uri),
        "DELETE" => client.delete(req.uri),
        method => {
            return Err(HttpClientError::BadMethod { method: method.into() });
        }
    };

    let request = request_builder
        .headers(deserialize_headers(req.headers))
        .body(req.body.clone())
        .build()
        .unwrap();

    let response = match client.execute(request).await {
        Ok(response) => response,
        Err(e) => {
            return Err(HttpClientError::RequestFailed { error: format!("{}", e) });
        }
    };

    let http_client_response = HttpClientResponse {
        status: response.status().as_u16(),
        headers: serialize_headers(&response.headers().clone()),
    };

    let message = KernelMessage {
        id,
        source,
        target,
        rsvp: None,
        message: Message::Response((Ok(
            Response {
                ipc: Some(serde_json::to_string(&http_client_response).unwrap()),
                metadata: None,
            }),
            None,
        )),
        payload: Some(Payload {
            mime: Some("application/json".into()),
            bytes: response.bytes().await.unwrap().to_vec(),
        }),
    };

    send_to_loop.send(message).await.unwrap();

    Ok(())
}

//
//  helpers
//
fn to_pascal_case(s: &str) -> String {
    s.split('-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect::<Vec<String>>()
        .join("-")
}

fn serialize_headers(headers: &HeaderMap) -> HashMap<String, String> {
    let mut hashmap = HashMap::new();
    for (key, value) in headers.iter() {
        let key_str = to_pascal_case(&key.to_string());
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

fn make_error_message(
    our_name: String,
    id: u64,
    source: Address,
    error: HttpClientError,
) -> KernelMessage {
    KernelMessage {
        id,
        source: source.clone(),
        target: Address {
            node: our_name.clone(),
            process: source.process.clone(),
        },
        rsvp: None,
        message: Message::Response((Err(UqbarError {
            kind: error.kind().into(),
            message: Some(serde_json::to_string(&error).unwrap()),  //  TODO: handle error?
        }), None)),
        payload: None,
    }
}

//  TODO: factor our with microkernel
fn get_current_unix_time() -> anyhow::Result<u64> {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(t) => Ok(t.as_secs()),
        Err(e) => Err(e.into()),
    }
}
