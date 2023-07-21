use crate::types::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use warp::{Reply, Filter};
use serde_json::{json, Map, Value};

/// http driver
pub async fn http_server(
  our: &String,
  message_rx: MessageReceiver,
  message_tx: MessageSender,
  print_tx: PrintSender,
) {
  let connections: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));

  tokio::join!(
    http_serve(our.clone(), connections.clone(), message_tx.clone(), print_tx.clone()),
    http_handle_messages(connections, message_rx, print_tx)
  );
}

async fn http_handle_messages(
  connections: Arc<Mutex<HashMap<String, String>>>,
  mut message_rx: MessageReceiver,
  print_tx: PrintSender,
) {
  while let Some(message_stack) = message_rx.recv().await {
    let stack_len = message_stack.len();
    let message = message_stack[stack_len - 1].clone();
    
    let Some(value) = message.payload.json.clone() else {
      panic!("http_server: request must have JSON payload, got: {:?}", message);
    };
    let request: HttpConnect = serde_json::from_value(value).unwrap();
    let mut routes_map = connections.lock().unwrap();
    routes_map.insert(request.path, request.app);
    let _ = print_tx.send(format!("connected app {:?}", routes_map)).await;
  }
}

async fn http_serve(
  our: String,
  connections: Arc<Mutex<HashMap<String, String>>>,
  message_tx: MessageSender,
  print_tx: PrintSender,
) {
  let filter = warp::filters::method::method()
    .and(warp::path::full())
    .and(warp::filters::header::headers_cloned())
    .and(warp::filters::body::json())
    .and(warp::any().map(move || our.clone()))
    .and(warp::any().map(move || connections.clone()))
    .and(warp::any().map(move || message_tx.clone()))
    .and(warp::any().map(move || print_tx.clone()))
    .and_then(|method, path: warp::path::FullPath, headers, body, our, connections: Arc<Mutex<HashMap<String, String>>>, message_tx, print_tx| async move {
      let target_app = connections.lock().unwrap().get(&path.as_str().to_string()).unwrap().to_string();
      // await message loop incoming here?

      handler(method, path, headers, body, our, target_app, message_tx, print_tx).await
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
  message_tx: MessageSender, print_tx: PrintSender

) -> Result<impl warp::Reply, warp::Rejection> {
  let path_str = path.as_str().to_string();
  // Return a response

  let json_payload: serde_json::Value = serde_json::json!(
    {
      "method": method.to_string(),
      "path": path_str,
      // "headers": headers, // TODO serialize to json not working
      "body": body
    }
  );

  let message = Message {
    message_type: MessageType::Request(false), // TODO true
    wire: Wire {
        source_ship: our.clone().to_string(),
        source_app: "http_server".to_string(),
        target_ship: our.clone().to_string(),
        target_app: target_app,
    },
    payload: Payload {
        json: Some(json_payload),
        bytes: None,
    },
  };

  message_tx.send(vec![message]).await.unwrap();

  Ok(warp::reply::html(format!(
      "Received a {} request for path {} with headers: {:?} and body: {:?}",
      method, path_str, headers, body
  )))
}
