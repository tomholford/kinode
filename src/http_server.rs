use crate::types::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use warp::{Reply, Filter};
use warp::http::{StatusCode, HeaderMap, header::HeaderName, header::HeaderValue};
use tokio::sync::oneshot;
use rand::{Rng, distributions::Alphanumeric};

pub type HttpSender = tokio::sync::oneshot::Sender<HttpResponse>;
pub type HttpResponseSenders = Arc<Mutex<HashMap<String, HttpSender>>>;

const ID_LENGTH: usize = 20;

/// http driver
pub async fn http_server(
  our: &String,
  message_rx: MessageReceiver,
  message_tx: MessageSender,
  print_tx: PrintSender,
) {
  let http_response_senders = Arc::new(Mutex::new(HashMap::new()));

  tokio::join!(
    http_serve(our.clone(), http_response_senders.clone(), message_tx.clone(), print_tx.clone()),
    http_handle_messages(http_response_senders, message_rx, print_tx)
  );
}

async fn http_handle_messages(
  http_response_senders: HttpResponseSenders,
  mut message_rx: MessageReceiver,
  print_tx: PrintSender,
) {
  while let Some(message_stack) = message_rx.recv().await {
    let stack_len = message_stack.len();
    let message = message_stack[stack_len - 1].clone();
    
    let Some(value) = message.payload.json.clone() else {
      panic!("http_server: action must have JSON payload, got: {:?}", message);
    };
    let request: HttpResponse = serde_json::from_value(value).unwrap();
    let _ = print_tx.send(format!("GOT A RESPONSE {:?}", request.id)).await;
    let channel = http_response_senders.lock().unwrap().remove(request.id.as_str()).unwrap();
    let _ = channel.send(HttpResponse {
      id: request.id,
      status: request.status,
      headers: request.headers,
      body: message.payload.bytes,
    });
    }
  }

async fn http_serve(
  our: String,
  http_response_senders: HttpResponseSenders,
  message_tx: MessageSender,
  print_tx: PrintSender,
) {
  let _ = print_tx.send(format!("GOT A REQUEST")).await;
  let filter = warp::filters::method::method()
    .and(warp::path::full())
    .and(warp::filters::header::headers_cloned())
    .and(warp::filters::body::bytes())
    .and(warp::any().map(move || our.clone()))
    .and(warp::any().map(move || http_response_senders.clone()))
    .and(warp::any().map(move || message_tx.clone()))
    .and(warp::any().map(move || print_tx.clone()))
    .and_then(handler);

  warp::serve(filter).run(([127, 0, 0, 1], 8080)).await;
}

async fn handler(
  method: warp::http::Method,
  path: warp::path::FullPath,
  headers: warp::http::HeaderMap,
  body: warp::hyper::body::Bytes,
  our: String,
  http_response_senders: HttpResponseSenders,
  message_tx: MessageSender,
  print_tx: PrintSender
) -> Result<impl warp::Reply, warp::Rejection> {
  let path_str = path.as_str().to_string();
  let id = create_id();
  let message = Message {
    message_type: MessageType::Request(true),
    wire: Wire {
      source_ship: our.clone().to_string(),
      source_app: "http_server".to_string(),
      target_ship: our.clone().to_string(),
      target_app: "http_bindings".to_string(),
    },
    payload: Payload {
      json: Some(serde_json::json!(
        {
          "action": "request".to_string(),
          "id": id,
          "method": method.to_string(),
          "path": path_str,
          "headers": serialize_headers(&headers),
        }
      )),
      bytes: Some(body.to_vec()), // TODO None sometimes
    },
  };

  let (response_sender, response_receiver) = oneshot::channel();
  http_response_senders.lock().unwrap().insert(id, response_sender);

  message_tx.send(vec![message]).await.unwrap();
  let from_channel = response_receiver.await.unwrap();
  let _ = print_tx.send("SENDING RESPONSE".to_string()).await;
  let reply = warp::reply::with_status(
    match from_channel.body {
      Some(val) => val,
      None => vec![],
    },
    StatusCode::from_u16(from_channel.status).unwrap()
  );
  let mut response = reply.into_response();

  // Merge the deserialized headers into the existing headers
  let existing_headers = response.headers_mut();
  for (header_name, header_value) in deserialize_headers(from_channel.headers).iter() {
    existing_headers.insert(header_name.clone(), header_value.clone());
  }
  let _ = print_tx.send("RIGHT BEFORE".to_string()).await;
  Ok(response)
}

//
//  helpers
//

fn create_id() -> String {
  rand::thread_rng()
    .sample_iter(&Alphanumeric)
    .take(ID_LENGTH)
    .map(char::from)
    .collect()
}

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