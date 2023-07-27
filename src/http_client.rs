use serde::{Serialize, Deserialize};
use crate::types::*;
use serde_json::json;

#[derive(Debug, Serialize, Deserialize)]
struct HttpClientRequest {
  uri: String,
  method: String,
  // headers: Vec<(String, String)>,
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
  let req: HttpClientRequest = serde_json::from_value(value).unwrap();

  print_tx.send(Printout {
    verbosity: 0,
    content: format!("http_client: got request: {:?}", req),
  }).await;

  let client = reqwest::Client::new();
        
  let request_builder = match req.method.to_uppercase()[..].to_string().as_str() {
      "GET" => client.get(req.uri),
      "POST" => client.post(req.uri),
      "PUT" => client.put(req.uri),
      "DELETE" => client.delete(req.uri),
      _ => panic!("Unsupported HTTP method: {}", req.method),
  };
  
  let request = request_builder
      // .headers(self.headers.clone())
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
  // !message tuna http_client {"method": "GET", "uri": "https://google.com", "body": ""}
}
