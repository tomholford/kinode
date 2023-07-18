use crate::types::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use warp::{Reply, Filter};

/// http driver
pub async fn http_server(
  message_rx: MessageReceiver,
  print_tx: PrintSender,
) {
  let routes: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));

  tokio::join!(
    http_serve(routes.clone()),
    http_bind(routes, message_rx, print_tx)
  );
}

async fn http_bind(
  routes: Arc<Mutex<HashMap<String, String>>>,
  mut message_rx: MessageReceiver,
  print_tx: PrintSender,
) {
  while let Some(message_stack) = message_rx.recv().await {
    let stack_len = message_stack.len();
    let message = message_stack[stack_len - 1].clone();
    let _ = print_tx.send(format!("in http_server message => {:?}", message)).await;
    
    let Some(value) = message.payload.json.clone() else {
      panic!("http_server: request must have JSON payload, got: {:?}", message);
    };
    // TODO should be HttpServerCommand
    let request: SetResponseFields = serde_json::from_value(value).unwrap();

    let mut routes_map = routes.lock().unwrap();
    routes_map.insert(request.path, request.content);
    let _ = print_tx.send(format!("asdf {:?}", routes_map)).await;
  }
}

async fn http_serve(
  gets: Arc<Mutex<HashMap<String, String>>>,
  // posts: Arc<Mutex<HashMap<String, String>>>
) {
  let get_filter = warp::path!(String)
    .and(warp::get())
    .and(warp::any().map(move || gets.clone()))
    .and_then(http_get_request);

  // let post_filter = warp::path!(String)
  //   .and(warp::get())
  //   .and(warp::any().map(move || posts.clone()))
  //   .and_then(http_post_request);
  
  // let filter = get_filter.or(post_filter)

  warp::serve(get_filter).run(([127, 0, 0, 1], 3030)).await;
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

// TODO send message to app
// async fn http_post_request(path: String, map: Arc<Mutex<HashMap<String, String>>>) -> Result<impl Reply, warp::Rejection> {
//   // Acquire the lock to access the HashMap
//   let guard = map.lock().unwrap();

//   // Check if the requested path exists in the HashMap
//   if let Some(content) = guard.get(&path) {
//       // If the path exists, create a Warp Response with the content
//       Ok(warp::reply::html(content.clone()))
//   } else {
//     // If the path does not exist, return a 404 Not Found response
//     let not_found_response = warp::reply::html("Not Found".to_string());
//     Ok(not_found_response)
//   }
// }
