use crate::types::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use warp::{Reply, Filter};

/// http driver
pub async fn http_server(
  our: &String,
  message_rx: MessageReceiver,
  message_tx: MessageSender,
  print_tx: PrintSender,
) {
  let gets: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
  let posts: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));

  tokio::join!(
    http_serve(our.clone(), gets.clone(), posts.clone(), message_tx.clone(), print_tx.clone()),
    http_handle_messages(gets, posts, message_rx, print_tx)
  );
}

async fn http_handle_messages(
  gets: Arc<Mutex<HashMap<String, String>>>,
  posts: Arc<Mutex<HashMap<String, String>>>,
  mut message_rx: MessageReceiver,
  print_tx: PrintSender,
) {
  while let Some(message_stack) = message_rx.recv().await {
    let stack_len = message_stack.len();
    let message = message_stack[stack_len - 1].clone();
    
    let Some(value) = message.payload.json.clone() else {
      panic!("http_server: request must have JSON payload, got: {:?}", message);
    };
    let request: HttpServerCommand = serde_json::from_value(value).unwrap();
    match request {
      HttpServerCommand::SetResponse(req) => {
        let mut routes_map = gets.lock().unwrap();
        routes_map.insert(req.path, req.content);
        let _ = print_tx.send(format!("set response {:?}", routes_map)).await;
      },
      HttpServerCommand::Connect(req) => {
        let mut routes_map = posts.lock().unwrap();
        routes_map.insert(req.path, req.app);
        let _ = print_tx.send(format!("connected app {:?}", routes_map)).await;
      }
    }
  }
}

async fn http_serve(
  our: String,
  gets: Arc<Mutex<HashMap<String, String>>>,
  posts: Arc<Mutex<HashMap<String, String>>>,
  message_tx: MessageSender,
  print_tx: PrintSender,
) {
  let get_filter = warp::path!(String)
    .and(warp::get())
    .and(warp::any().map(move || gets.clone()))
    .and_then(http_get_request);

  let post_filter = warp::path!(String)
    .and(warp::post())
    .and(warp::body::json())
    .and(warp::any().map(move || our.clone()))
    .and(warp::any().map(move || posts.clone()))
    .and(warp::any().map(move || message_tx.clone()))
    .and(warp::any().map(move || print_tx.clone()))
    .and_then(|path, data, our, posts: Arc<Mutex<HashMap<String, String>>>, message_tx, print_tx| async move {
      let target_app = posts.lock().unwrap().get(&path).unwrap().to_string();

      http_post_request(path, data, our, target_app, message_tx, print_tx).await
  });  
  let filter = get_filter.or(post_filter);

  warp::serve(filter).run(([127, 0, 0, 1], 3030)).await;
}

// Handler function to serve content for a given path
async fn http_get_request(path: String, map: Arc<Mutex<HashMap<String, String>>>) -> Result<impl Reply, warp::Rejection> {
  // Acquire the lock to access the HashMap
  let guard = map.lock().unwrap();
  // Check if the requested path exists in the HashMap
  if let Some(content) = guard.get(&path) {
      // If the path exists, create a Warp Response with the content
      Ok(warp::reply::html(content.clone()))
  } else {
    // If the path does not exist, return a 404 Not Found response
    let not_found_response = warp::reply::html("Not Found".to_string());
    Ok(not_found_response)
  }
}

async fn http_post_request(
  path: String,
  data: serde_json::Value,
  our: String,
  target_app: String,
  message_tx: MessageSender, print_tx: PrintSender
) -> Result<impl warp::Reply, warp::Rejection> {
  let message = Message {
      message_type: MessageType::Request(false),
      wire: Wire {
          source_ship: our.clone().to_string(),
          source_app: "http_server".to_string(),
          target_ship: our.clone().to_string(),
          target_app: target_app,
      },
      payload: Payload {
          json: Some(data),
          bytes: None,
      },
  };

  message_tx.send(vec![message]).await.unwrap();

  // TODO actually have to let the app generate the response, which means
  // waiting for a response in the event loop...kind of annoying...
  Ok(warp::reply::json(&"Message sent successfully"))
}