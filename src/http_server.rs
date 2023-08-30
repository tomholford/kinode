use crate::types::*;
 use serde_urlencoded;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use warp::http::{header::HeaderName, header::HeaderValue, HeaderMap, StatusCode};
use warp::{Filter, Reply};

type HttpSender = tokio::sync::oneshot::Sender<HttpResponse>;
type HttpResponseSenders = Arc<Mutex<HashMap<u64, HttpSender>>>;

/// http driver
pub async fn http_server(
    our: String,
    our_port: u16,
    mut recv_in_server: MessageReceiver,
    send_to_loop: MessageSender,
    print_tx: PrintSender,
) {
    let http_response_senders = Arc::new(Mutex::new(HashMap::new()));
    let our_name = our.clone();

    tokio::join!(
        http_serve(
            our.clone(),
            our_port,
            http_response_senders.clone(),
            send_to_loop.clone(),
            print_tx.clone()
        ),
        async move {
            while let Some(km) = recv_in_server.recv().await {
                let KernelMessage {
                    id,
                    target: _,
                    rsvp: _,
                    message: Ok(TransitMessage::Response(TransitPayload {
                        source,
                        json,
                        bytes,
                    })),
                } = km.clone() else {
                    panic!("http_server: bad message");
                };

                if let Err(e) = http_handle_messages(
                    http_response_senders.clone(),
                    id,
                    json,
                    bytes,
                    print_tx.clone()
                ).await {
                    send_to_loop.send(
                        make_error_message(
                            our_name.clone(),
                            id.clone(),
                            source.clone().identifier,
                            e
                        )
                    ).await.unwrap();
                }
            }
        }
    );
}

async fn http_handle_messages(
    http_response_senders: HttpResponseSenders,
    id: u64,
    json: Option<String>,
    bytes: TransitPayloadBytes,
    _print_tx: PrintSender,
) -> Result<(), HttpServerError> {
    let Some(ref json) = json else {
        return Err(HttpServerError::NoJson);
    };
    let request  = match serde_json::from_str::<HttpResponse>(json) {
        Ok(r) => r,
        Err(e) => {
            return Err(HttpServerError::BadJson {
                json: json.clone(),
                error: format!("{}", e),
            });
        }
    };

    let TransitPayloadBytes::Some(bytes) = bytes else {
        return Err(HttpServerError::NoBytes)
    };

    let mut senders = http_response_senders.lock().await;
    match senders.remove(&id) {
        Some(channel) => {
            let _ = channel.send(HttpResponse {
                status: request.status,
                headers: request.headers,
                body: Some(bytes.clone()),
            });
        }
        None => {
            panic!("http_server: inconsistent state, no key found for id {}", id);
        }
    }

    Ok(())
}

async fn http_serve(
    our: String,
    our_port: u16,
    http_response_senders: HttpResponseSenders,
    send_to_loop: MessageSender,
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
        .and(warp::any().map(move || send_to_loop.clone()))
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
    send_to_loop: MessageSender,
    _print_tx: PrintSender,
) -> Result<impl warp::Reply, warp::Rejection> {
    let path_str = path.as_str().to_string();
    let id: u64 = rand::random();
    let message = KernelMessage {
        id: id.clone(),
        target: ProcessReference {
            node: our.clone(),
            identifier: ProcessIdentifier::Name("http_bindings".into()),
        },
        rsvp: None, // TODO I believe this is correct
        message: Ok(TransitMessage::Request(TransitRequest {
            is_expecting_response: true,
            payload: TransitPayload {
                source: ProcessReference {
                    node: our.clone(),
                    identifier: ProcessIdentifier::Name("http_server".into()),
                },
                json: Some(serde_json::json!({
                    "action": "request".to_string(),
                    "method": method.to_string(),
                    "path": path_str,
                    "headers": serialize_headers(&headers),
                    "query_params": query_params,
                }).to_string()),
                bytes: TransitPayloadBytes::Some(body.to_vec()), // TODO None sometimes
            },
        })),
    };

    let (response_sender, response_receiver) = oneshot::channel();
    http_response_senders
        .lock()
        .await
        .insert(id, response_sender);

    send_to_loop.send(message).await.unwrap();
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

fn make_error_message(
    our_name: String,
    id: u64,
    source: ProcessIdentifier,
    error: HttpServerError,
) -> KernelMessage {
    KernelMessage {
        id,
        target: ProcessReference {
            node: our_name.clone(),
            identifier: source,
        },
        rsvp: None,
        message: Err(UqbarError {
            source: ProcessReference {
                node: our_name,
                identifier: ProcessIdentifier::Name("http_client".into()),
            },
            timestamp: get_current_unix_time().unwrap(),  //  TODO: handle error?
            payload: UqbarErrorPayload {
                kind: error.kind().into(),
                message: serde_json::to_value(error).unwrap(),  //  TODO: handle error?
                context: serde_json::to_value("").unwrap(),
            },
        }),
    }
}

//  TODO: factor our with microkernel
fn get_current_unix_time() -> anyhow::Result<u64> {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(t) => Ok(t.as_secs()),
        Err(e) => Err(e.into()),
    }
}
