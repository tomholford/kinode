use serde::{Serialize, Deserialize};
use crate::types::*;
use serde_json::json;
use std::collections::HashMap;
use http::header::{HeaderName, HeaderValue, HeaderMap};

#[derive(Debug, Serialize, Deserialize)]
struct HttpClientRequest {
  uri: String,
  method: String,
  headers: HashMap<String, String>,
  body: String,
}

pub async fn http_client(
  our_name: &str,
  send_to_loop: MessageSender,
  mut recv_in_client: MessageReceiver,
  print_tx: PrintSender,
) {
  while let Some(message) = recv_in_client.recv().await {
    tokio::spawn(
      handle_message(
        our_name.to_string(),
        send_to_loop.clone(),
        message,
        print_tx.clone()
      )
    );
  }
}

async fn handle_message(
  our: String,
  send_to_loop: MessageSender,
  wm: WrappedMessage,
  print_tx: PrintSender,
) {
  let Some(value) = wm.message.payload.json.clone() else {
    panic!("http_client: request must have JSON payload, got: {:?}", wm.message);
  };

  let req: HttpClientRequest = match serde_json::from_value(value) {
    Ok(req) => req,
    Err(e) => panic!("http_client: failed to parse request: {:?}", e),
  };

  print_tx.send(Printout {
    verbosity: 0,
    content: format!("http_client: got request: {:?}", req),
  }).await;

  let client = reqwest::Client::new();
        
  let request_builder = match req.method.to_uppercase()[..].to_string().as_str() {
      "GET" =>    client.get(req.uri),
      "PUT" =>    client.put(req.uri),
      "POST" =>   client.post(req.uri),
      "DELETE" => client.delete(req.uri),
      _ => panic!("Unsupported HTTP method: {}", req.method),
  };

  // parse headers
  let mut header_map = HeaderMap::new();
  for (name, value) in req.headers {
      let header_name = HeaderName::from_bytes(name.as_bytes())
          .expect("Failed to convert header name to HeaderName.");
      let header_value = HeaderValue::from_str(&value.as_str())
          .expect("Failed to convert header value to HeaderValue.");
      header_map.insert(header_name, header_value);
  }
  
  let request = request_builder
      .headers(header_map)
      .body(req.body.clone())
      .build().unwrap();
  
  let response = client.execute(request).await.unwrap(); // TODO error handling
  
  if response.status().is_success() {
      let body = response.text().await.unwrap(); // TODO error handling
      print_tx.send(Printout {
        verbosity: 0,
        content: format!("http_client: got response: {:?}", body),
      }).await;
  } else {
      // Err(Error::from(response.error_for_status().unwrap_err()))
      panic!("foo")
  }
  // !message tuna http_client {"method": "GET", "uri": "https://jsonplaceholder.typicode.com/posts", "headers": {}, "body": ""}
  // !message tuna http_client {"method": "POST", "uri": "https://jsonplaceholder.typicode.com/posts", "headers": {"Content-Type": "application/json"}, "body": "{\"title\": \"foo\", \"body\": \"bar\"}"}
  // !message tuna http_client {"method": "PUT", "uri": "https://jsonplaceholder.typicode.com/posts", "body": "{\"title\": \"foo\", \"body\": \"bar\"}"}

}
