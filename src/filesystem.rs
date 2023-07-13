use http::Uri;
use tokio::fs;
use tokio_tungstenite::tungstenite::Error;

use crate::types::*;

pub async fn fs_sender(
    our_name: &str,
    message_tx: MessageSender,
    print_tx: PrintSender,
    mut rx: MessageReceiver
) {
    while let Some(message) = rx.recv().await {
        tokio::spawn(
            handle_read(
                our_name.to_string(),
                message,
                message_tx.clone(),
                print_tx.clone()
            )
        );
    }
}

async fn handle_read(
    our_name: String,
    message: Message,
    message_tx: MessageSender,
    print_tx: PrintSender,
) -> Result<(), Error> {
    // if our_name != message.source.server {
    //     panic!("filesystem: request must come from our_name={}, got: {:?}", our_name, message);
    // }
    if "filesystem".to_string() != message.wire.target_app {
        panic!("filesystem: filesystem must be target.app, got: {:?}", message);
    }
    let Payload::Json(value) = message.payload else {
        panic!("filesystem: request must have JSON payload, got: {:?}", message);
    };
    let serde_json::Value::String(ref uri_string) = value["uri"] else {
        panic!("filesystem: request must have string payload, got: {:?}", value);
    };
    let uri = uri_string.parse::<Uri>().unwrap();
    if Some("fs") != uri.scheme_str() {
        panic!("filesystem: uri scheme must be uri, got: {:?}", uri.scheme_str());
    }

    let file_contents = fs::read(uri.host().unwrap()).await?;
    let _ = print_tx.send(
        format!(
            "filesystem: got file at {} of size {}",
            uri.host().unwrap(),
            file_contents.len()
            )
    ).await;

    let response = Message {
        note: Note::Give,
        wire: Wire {
            source_ship: our_name.clone(),
            source_app: "filesystem".to_string(),
            target_ship: our_name.clone(),
            target_app: message.wire.source_app,
        },
        payload: Payload::Bytes(file_contents),
    };

    let _ = message_tx.send(response).await;

    Ok(())
}
