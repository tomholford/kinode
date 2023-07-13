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

//  TODO: error handling: should probably send error messages to caller
async fn handle_read(
    our_name: String,
    message: Message,
    to_event_loop: MessageSender,
    print_tx: PrintSender,
) -> Result<(), Error> {
    // if our_name != message.source.server {
    //     panic!("filesystem: request must come from our_name={}, got: {:?}", our_name, message);
    // }
    if "filesystem".to_string() != message.wire.target_app {
        panic!("filesystem: filesystem must be target.app, got: {:?}", message);
    }
    let Some(value) = message.payload.json else {
        panic!("filesystem: request must have JSON payload, got: {:?}", message);
    };

    let request: FileSystemCommand = serde_json::from_value(value).unwrap();
    let uri = request.uri_string.parse::<Uri>().unwrap();
    if Some("fs") != uri.scheme_str() {
        panic!("filesystem: uri scheme must be uri, got: {:?}", uri.scheme_str());
    }

    let response_payload = match request.command {
        FileSystemAction::Read => {
            let file_contents = fs::read(uri.host().unwrap()).await?;
            let _ = print_tx.send(
                format!(
                    "filesystem: got file at {} of size {}",
                    uri.host().unwrap(),
                    file_contents.len()
                    )
            ).await;

            Payload {
                json: None,
                bytes: Some(file_contents),
            }
        },
        FileSystemAction::Write => {
            let Some(payload_bytes) = message.payload.bytes else {
                panic!("filesystem: received Write without any bytes to append");  //  TODO: change to an error response once responses are real
            };

            fs::write(uri.host().unwrap(), &payload_bytes).await?;

            Payload {
                json: Some(serde_json::Value::Null),  //  TODO: add real response once responses are real
                bytes: None,
            }
        },
        FileSystemAction::Append => {
            let Some(mut payload_bytes) = message.payload.bytes else {
                panic!("filesystem: received Append without any bytes to append");  //  TODO: change to an error response once responses are real
            };
            let mut file_contents = fs::read(uri.host().unwrap()).await?;
            let _ = print_tx.send(
                format!(
                    "filesystem: got file at {} of size {}",
                    uri.host().unwrap(),
                    file_contents.len()
                    )
            ).await;

            file_contents.append(&mut payload_bytes);

            fs::write(uri.host().unwrap(), &file_contents).await?;

            Payload {
                json: Some(serde_json::Value::Null),  //  TODO: add real response once responses are real
                bytes: None,
            }
        },
    };

    let response = Message {
        message_type: MessageType::Response,
        wire: Wire {
            source_ship: our_name.clone(),
            source_app: "filesystem".to_string(),
            target_ship: our_name.clone(),
            target_app: message.wire.source_app,
        },
        payload: response_payload,
    };

    let _ = to_event_loop.send(response).await;

    Ok(())
}
