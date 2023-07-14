use http::Uri;
use tokio::fs;
use tokio_tungstenite::tungstenite::Error;

use crate::types::*;

pub async fn fs_sender(
    our_name: &str,
    send_to_loop: MessageSender,
    print_tx: PrintSender,
    mut recv_in_fs: MessageReceiver
) {
    while let Some(message) = recv_in_fs.recv().await {
        tokio::spawn(
            handle_read(
                our_name.to_string(),
                message,
                send_to_loop.clone(),
                print_tx.clone()
            )
        );
    }
}

//  TODO: error handling: should probably send error messages to caller
async fn handle_read(
    our_name: String,
    mut messages: MessageStack,
    send_to_loop: MessageSender,
    print_tx: PrintSender,
) -> Result<(), Error> {
    let Some(message) = messages.pop() else {
        panic!("filesystem: filesystem must receive non-empty MessageStack, got: {:?}", messages);
    };
    // if our_name != message.source.server {
    //     panic!("filesystem: request must come from our_name={}, got: {:?}", our_name, message);
    // }
    if "filesystem".to_string() != message.wire.target_app {
        panic!("filesystem: filesystem must be target.app, got: {:?}", message);
    }
    let Some(value) = message.payload.json.clone() else {
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
            let Some(payload_bytes) = message.payload.bytes.clone() else {
                panic!("filesystem: received Write without any bytes to append");  //  TODO: change to an error response once responses are real
            };

            fs::write(uri.host().unwrap(), &payload_bytes).await?;

            Payload {
                json: Some(serde_json::Value::Null),  //  TODO: add real response once responses are real
                bytes: None,
            }
        },
        FileSystemAction::Append => {
            let Some(mut payload_bytes) = message.payload.bytes.clone() else {
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
            target_app: message.wire.source_app.clone(),
        },
        payload: response_payload,
    };

    messages.push(message);
    messages.push(response);

    let _ = send_to_loop.send(messages).await;

    Ok(())
}
