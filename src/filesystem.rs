use http::Uri;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::fs;
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Error;

use crate::types::*;

pub async fn fs_sender(
    our_name: &str,
    send_to_loop: MessageSender,
    print_tx: PrintSender,
    mut recv_in_fs: MessageReceiver
) {
    //  TODO: store or back up in DB/kv?
    let read_access_by_node: Arc<RwLock<HashMap<String, HashSet<String>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    while let Some(message) = recv_in_fs.recv().await {
        tokio::spawn(
            handle_request(
                our_name.to_string(),
                message,
                Arc::clone(&read_access_by_node),
                send_to_loop.clone(),
                print_tx.clone()
            )
        );
    }
}

fn get_file_path(uri_string: &str) -> String {
    let uri = uri_string.parse::<Uri>().unwrap();
    if Some("fs") != uri.scheme_str() {
        panic!("filesystem: uri scheme must be uri, got: {:?}", uri.scheme_str());
    }
    uri.host().unwrap().to_string()
}

//  TODO: error handling: should probably send error messages to caller
async fn handle_request(
    our_name: String,
    mut messages: MessageStack,
    read_access_by_node: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    send_to_loop: MessageSender,
    print_tx: PrintSender,
) -> Result<(), Error> {
    let Some(message) = messages.pop() else {
        panic!("filesystem: filesystem must receive non-empty MessageStack, got: {:?}", messages);
    };
    if "filesystem".to_string() != message.wire.target_app {
        panic!("filesystem: filesystem must be target.app, got: {:?}", message);
    }
    let Some(value) = message.payload.json.clone() else {
        panic!("filesystem: request must have JSON payload, got: {:?}", message);
    };

    let request: FileSystemRequest = serde_json::from_value(value).unwrap();
    let source_ship = message.wire.source_ship.clone();

    let response_payload = match request {
        FileSystemRequest::Read(uri_string) => {
            let file_path = get_file_path(&uri_string);
            if our_name != source_ship {
                let read_access_by_node = read_access_by_node.read().await;
                let Some(allowed_files) = read_access_by_node.get(&source_ship) else {
                    panic!(
                        "filesystem: node {} not allowed to Read any files, got: {:?}",
                        source_ship,
                        message,
                    );
                };
                if !allowed_files.contains(&file_path) {
                    panic!(
                        "filesystem: node {} not allowed to Read {}, got: {:?}",
                        source_ship,
                        file_path,
                        message,
                    );
                }
            }
            let file_contents = fs::read(&file_path).await?;
            let _ = print_tx.send(
                format!(
                    "filesystem: got file at {} of size {}",
                    file_path,
                    file_contents.len()
                    )
            ).await;

            Payload {
                json: Some(json![{"uri_string": uri_string}]),  //  TODO: error propagation to caller
                bytes: Some(file_contents),
            }
        },
        FileSystemRequest::Write(uri_string) => {
            if our_name != source_ship {
                panic!("filesystem: Write request must come from our_name={}, got: {:?}", our_name, message);
            }
            let Some(payload_bytes) = message.payload.bytes.clone() else {
                panic!("filesystem: received Write without any bytes to append");  //  TODO: change to an error response once responses are real
            };

            let file_path = get_file_path(&uri_string);

            fs::write(&file_path, &payload_bytes).await?;

            Payload {
                json: Some(json![{"uri_string": uri_string}]),  //  TODO: error propagation to caller
                bytes: None,
            }
        },
        FileSystemRequest::Append(uri_string) => {
            if our_name != source_ship {
                panic!(
                    "filesystem: Append request must come from our_name={}, got: {:?}",
                    our_name,
                    message
                );
            }
            let Some(mut payload_bytes) = message.payload.bytes.clone() else {
                panic!("filesystem: received Append without any bytes to append");  //  TODO: change to an error response once responses are real
            };

            let file_path = get_file_path(&uri_string);

            let mut file_contents = fs::read(&file_path).await?;
            let _ = print_tx.send(
                format!(
                    "filesystem: got file at {} of size {}",
                    file_path,
                    file_contents.len()
                    )
            ).await;

            file_contents.append(&mut payload_bytes);

            fs::write(file_path, &file_contents).await?;

            Payload {
                json: Some(json![{"uri_string": uri_string}]),  //  TODO: error propagation to caller
                bytes: None,
            }
        },
        FileSystemRequest::AlterReadPermissions(uri_strings) => {
            let mut read_access_by_node = read_access_by_node.write().await;
            let mut allowed_files: HashSet<String> = match read_access_by_node.remove(&source_ship) {
                Some(allowed_files) => allowed_files,
                None => HashSet::new(),
            };

            for uri_string in &uri_strings {
                let file_path = get_file_path(uri_string);
                allowed_files.insert(file_path.to_string());
            }

            read_access_by_node.insert(source_ship.clone(), allowed_files);

            let mut file_paths: Vec<String> = read_access_by_node
                .get(&source_ship)
                .unwrap()
                .iter()
                .cloned()
                .collect();
            file_paths.sort();
            Payload {
                json: Some(json![{"file_paths": file_paths}]),
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
