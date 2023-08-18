use crate::types::*;
use serde::{Deserialize, Serialize};
use serde_urlencoded;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use warp::http::{header::HeaderName, header::HeaderValue, HeaderMap, StatusCode};
use warp::{Filter, Reply};

// types and constants
#[derive(Debug, Serialize, Deserialize)]
struct HttpResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Option<Vec<u8>>, // TODO does this use a lot of memory?
}
type HttpSender = tokio::sync::oneshot::Sender<HttpResponse>;
type HttpResponseSenders = Arc<Mutex<HashMap<u64, HttpSender>>>;

/// http driver
pub async fn http_server(
    our: String,
    our_port: u16,
    message_rx: MessageReceiver,
    message_tx: MessageSender,
    print_tx: PrintSender,
) {
    let http_response_senders = Arc::new(Mutex::new(HashMap::new()));


    tokio::join!(
        http_serve(
            our.clone(),
            our_port,
            http_response_senders.clone(),
            message_tx.clone(),
            print_tx.clone()
        ),
        http_handle_messages(http_response_senders, message_rx, print_tx)
    );
}

async fn http_handle_messages(
    http_response_senders: HttpResponseSenders,
    mut message_rx: MessageReceiver,
    print_tx: PrintSender,
) {
    while let Some(wm) = message_rx.recv().await {
        let WrappedMessage { ref id, target: _, rsvp: _, message: Ok(Message { source: _, ref content }), }
                = wm else {
            panic!("filesystem: unexpected Error")  //  TODO: implement error handling
        };

        let Some(value) = content.payload.json.clone() else {
            panic!("http_server: action must have JSON payload, got: {:?}", wm);
        };
        let request: HttpResponse = serde_json::from_value(value).unwrap();

        let mut senders = http_response_senders.lock().await;
        match senders.remove(id) {
            Some(channel) => {
                let _ = channel.send(HttpResponse {
                    status: request.status,
                    headers: request.headers,
                    body: content.payload.bytes.content.clone(),  //  TODO: ? ; could remove ref in 51 and avoid clone
                });
            }
            None => {
                // TODO: this should be a panic because something has gotten incredibly out of sync
                let _ = print_tx
                    .send(Printout {
                        verbosity: 1,
                        content: format!("http_server: NO KEY FOUND FOR ID {}", id),
                    })
                    .await;
            }
        }
    }
}

async fn http_serve(
    our: String,
    our_port: u16,
    http_response_senders: HttpResponseSenders,
    message_tx: MessageSender,
    print_tx: PrintSender,
) {
    let print_tx_move = print_tx.clone();
    let filter = warp::filters::method::method()
        .and(warp::path::full())
        .and(warp::filters::header::headers_cloned())
        .and(
            warp::filters::query::raw()
                .or(warp::any().map(|| String::default()))
                .unify()
                .map(|query_string: String| {
                    if query_string.is_empty() {
                        HashMap::new()
                    } else {
                        match serde_urlencoded::from_str(&query_string) {
                            Ok(map) => map,
                            Err(_) => HashMap::new(),
                        }
                    }
                }),
        )
        .and(warp::filters::body::bytes())
        .and(warp::any().map(move || our.clone()))
        .and(warp::any().map(move || http_response_senders.clone()))
        .and(warp::any().map(move || message_tx.clone()))
        .and(warp::any().map(move || print_tx_move.clone()))
        .and_then(handler);

    let _ = print_tx
        .send(Printout {
            verbosity: 1,
            content: format!("http_server: running on: {}", our_port),
        })
        .await;
    warp::serve(filter).run(([0, 0, 0, 0], our_port)).await;
}

async fn handler(
    method: warp::http::Method,
    path: warp::path::FullPath,
    headers: warp::http::HeaderMap,
    query_params: HashMap<String, String>,
    body: warp::hyper::body::Bytes,
    our: String,
    http_response_senders: HttpResponseSenders,
    message_tx: MessageSender,
    _print_tx: PrintSender,
) -> Result<impl warp::Reply, warp::Rejection> {
    let path_str = path.as_str().to_string();
    let id: u64 = rand::random();
    let message = WrappedMessage {
        id: id.clone(),
        target: ProcessNode {
            node: our.clone(),
            process: "http_bindings".into(),
        },
        rsvp: None, // TODO I believe this is correct
        message: Ok(Message {
            source: ProcessNode {
                node: our.clone(),
                process: "http_server".into(),
            },
            content: MessageContent {
                message_type: MessageType::Request(true),
                payload: Payload {
                    json: Some(serde_json::json!(
                      {
                        "action": "request".to_string(),
                        "method": method.to_string(),
                        "path": path_str,
                        "headers": serialize_headers(&headers),
                        "query_params": query_params,
                      }
                    )),
                    bytes: PayloadBytes {
                        circumvent: Circumvent::False,
                        content: Some(body.to_vec()), // TODO None sometimes
                    },
                },
            },
        }),
    };

    let (response_sender, response_receiver) = oneshot::channel();
    http_response_senders
        .lock()
        .await
        .insert(id, response_sender);

    message_tx.send(message).await.unwrap();
    let from_channel = response_receiver.await.unwrap();
    let reply = warp::reply::with_status(
        match from_channel.body {
            Some(val) => val,
            None => vec![],
        },
        StatusCode::from_u16(from_channel.status).unwrap(),
    );
    let mut response = reply.into_response();

    // Merge the deserialized headers into the existing headers
    let existing_headers = response.headers_mut();
    for (header_name, header_value) in deserialize_headers(from_channel.headers).iter() {
        existing_headers.insert(header_name.clone(), header_value.clone());
    }
    Ok(response)
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

pub async fn find_open_port(start_at: u16) -> Option<u16> {
    for port in start_at..=u16::MAX {
        let bind_addr = format!("0.0.0.0:{}", port);
        if is_port_available(&bind_addr).await {
            return Some(port);
        }
    }
    None
}

async fn is_port_available(bind_addr: &str) -> bool {
    match TcpListener::bind(bind_addr).await {
        Ok(_) => true,
        Err(_) => false,
    }
}
