use crate::types::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use warp::{Filter, Rejection, Reply};
use tokio::task;

/// http driver
pub async fn http_server(
  routes: Arc<Mutex<HashMap<String, DynamicRoute>>>,
  message_rx: MessageReceiver,
  print_tx: PrintSender,
) {
  // Dummy route for "Hello" variant
  let hello_route = warp::path("hello")
      .map(|| DynamicRoute::Hello);

  // Route for "Greet" variant with a dynamic segment
  let greet_route = warp::path!("greet" / String)
      .map(|name: String| DynamicRoute::Greet(name));

  // Combine both routes into a single filter
  let all_routes = hello_route.or(greet_route);

  // Add the routes to the hashmap
  let mut routes_map = routes.lock().unwrap();
  routes_map.insert("hello".to_string(), DynamicRoute::Hello);
  routes_map.insert("greet".to_string(), DynamicRoute::Greet("John".to_string()));
  
  tokio::join!(
    warp::serve(all_routes).run(([127, 0, 0, 1], 3030)),
    http_bind(message_rx, print_tx)
  );
}

async fn http_bind(
  mut message_rx: MessageReceiver,
  print_tx: PrintSender,
) {
  while let Some(message_stack) = message_rx.recv().await {
    let stack_len = message_stack.len();
    let message = message_stack[stack_len - 1].clone();
    print_tx.send(format!("in http_server message => {:?}", message)).await;
  }
}
