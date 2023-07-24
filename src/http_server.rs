use crate::types::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use warp::{Reply, Filter};
use serde_json::{json, Map, Value};
use tokio::sync::oneshot;

pub type HttpSender = tokio::sync::oneshot::Sender<HttpResponse>;
pub type HttpReceiver = tokio::sync::oneshot::Receiver<HttpResponse>;
pub type HttpResponseSenders = Arc<Mutex<HashMap<String, HttpSender>>>; // TODO uuid?

/// http driver
pub async fn http_server(
  our: &String,
  message_rx: MessageReceiver,
  message_tx: MessageSender,
  print_tx: PrintSender,
) {
  let connections: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
  let http_response_senders = Arc::new(Mutex::new(HashMap::new()));

  tokio::join!(
    http_serve(our.clone(), connections.clone(), http_response_senders.clone(), message_tx.clone(), print_tx.clone()),
    http_handle_connections(connections, http_response_senders, message_rx, print_tx)
  );
}

async fn http_handle_connections(
  connections: Arc<Mutex<HashMap<String, String>>>,
  http_response_senders: HttpResponseSenders,
  mut message_rx: MessageReceiver,
  print_tx: PrintSender,
) {
  while let Some(message_stack) = message_rx.recv().await {
    let stack_len = message_stack.len();
    let message = message_stack[stack_len - 1].clone();
    
    let Some(value) = message.payload.json.clone() else {
      panic!("http_server: request must have JSON payload, got: {:?}", message);
    };
    let request: HttpAction = serde_json::from_value(value).unwrap();

    match request {
      HttpAction::HttpConnect(act) => {
        let mut routes_map = connections.lock().unwrap();
        routes_map.insert(act.path, act.app);
        let _ = print_tx.send(format!("connected app {:?}", routes_map)).await;
      },
      HttpAction::HttpResponse(act) => {
        // if the request is a response => send it on the channel here
        let asdf = http_response_senders.lock().unwrap().remove("TODO ID HERE").unwrap();
        println!("{:?}", asdf);
        println!("got an HTTP response!");
        let _ = asdf.send(HttpResponse {
          id: "TODO ID HERE".to_string(),
          status: "201".to_string(),
          headers: "test headers".to_string(),
          body: "<h1>this is my response</h1>".to_string(),
        });
      }
    }
  }
}

async fn http_serve(
  our: String,
  connections: Arc<Mutex<HashMap<String, String>>>,
  http_response_senders: HttpResponseSenders,
  message_tx: MessageSender,
  print_tx: PrintSender,
) {
  let filter = warp::filters::method::method()
    .and(warp::path::full())
    .and(warp::filters::header::headers_cloned())
    .and(warp::filters::body::json())
    .and(warp::any().map(move || our.clone()))
    .and(warp::any().map(move || connections.clone()))
    .and(warp::any().map(move || http_response_senders.clone()))
    .and(warp::any().map(move || message_tx.clone()))
    .and(warp::any().map(move || print_tx.clone()))
    .and_then(|
        method,
        path: warp::path::FullPath,
        headers,
        body,
        our,
        connections: Arc<Mutex<HashMap<String, String>>>,
        http_response_senders,
        message_tx,
        print_tx
      | async move {
      let target_app = connections.lock().unwrap().get(&path.as_str().to_string()).unwrap().to_string();
      handler(method, path, headers, body, our, target_app, http_response_senders, message_tx, print_tx).await
    });

  warp::serve(filter).run(([127, 0, 0, 1], 8080)).await;
}

async fn handler(
  method: warp::http::Method,
  path: warp::path::FullPath,
  headers: warp::http::HeaderMap,
  body: serde_json::Value,
  our: String,
  target_app: String,
  http_response_senders: HttpResponseSenders,
  message_tx: MessageSender,
  print_tx: PrintSender

) -> Result<impl warp::Reply, warp::Rejection> {
  let path_str = path.as_str().to_string();
  let message = Message {
    message_type: MessageType::Request(true), // TODO no idea what this actually does...
    wire: Wire {
      source_ship: our.clone().to_string(),
      source_app: "http_server".to_string(),
      target_ship: our.clone().to_string(),
      target_app: target_app,
    },
    payload: Payload {
      json: Some(serde_json::json!(
        {
          "method": method.to_string(),
          "path": path_str,
          // "headers": headers, // TODO serialize to json not working
          "body": body
        }
      )),
      bytes: None,
    },
  };

  let (response_sender, response_receiver) = oneshot::channel();
  http_response_senders.lock().unwrap().insert("TODO ID HERE".to_string(), response_sender);

  message_tx.send(vec![message]).await.unwrap();
  let response = response_receiver.await;
  // TODO send repsonse to outsdie world
  println!("RESPONSE IN HANDLER: {:?}", response);
  Ok(warp::reply::html(format!(
      "Received a {} request for path {} with headers: {:?} and body: {:?}",
      method, path_str, headers, body
  )))
}
