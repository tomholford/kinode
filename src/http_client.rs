use serde::{Serialize, Deserialize};
use crate::types::*;
use serde_json::json;


pub async fn http_client(
  our_name: &str,
  send_to_loop: MessageSender,
  mut recv_in_client: MessageReceiver,
  print_tx: PrintSender,
) {
  while let Some(message) = recv_in_client.recv().await {
    tokio::spawn(
      handle_request(
        our_name.to_string(),
        send_to_loop.clone(),
        message,
        print_tx.clone()
      )
    );
  }
}

async fn handle_request(
  our: String,
  send_to_loop: MessageSender,
  wm: WrappedMessage,
  print_tx: PrintSender,
) {
  let Some(value) = wm.message.payload.json.clone() else {
    panic!("keygen: request must have JSON payload, got: {:?}", wm.message);
  };
  let act: serde_json::Value = serde_json::from_value(value).unwrap();

  print_tx.send(Printout {
    verbosity: 0,
    content: format!("http_client: got request: {:?}", act),
  }).await;

  let res = reqwest::get("https://google.com").await.unwrap();
  let body = res.text().await.unwrap();
  print_tx.send(Printout {
    verbosity: 0,
    content: format!("http_client: got response: {:?}", body),
  }).await;
  // !message tuna http_client "foo"
}
