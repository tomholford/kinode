use anyhow::Result;
use bytes::Bytes;
use http::Uri;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::sync::Mutex;
// use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Error;

use crate::types::*;

lazy_static::lazy_static! {
    static ref HAS_FULL_HOME_ACCESS: HashSet<String> = vec![
        "filesystem".to_string(),
        "kernel".to_string(),
        "process_manager".to_string(),
        "terminal".to_string(),
    ].into_iter().collect();
}

#[derive(Eq, Hash, PartialEq)]
enum FileMode {
    Read,
    Append,
}
#[derive(Eq, Hash, PartialEq)]
struct FileRef {
    path: String,
    mode: FileMode,
}

async fn create_dir_if_dne(path: &str) -> Result<()> {
    if let Err(_) = fs::read_dir(&path).await {
        fs::create_dir_all(&path).await?;
        Ok(())
    } else {
        Ok(())
    }
}

async fn get_file_path(uri_string: &str) -> String {
    let uri = uri_string.parse::<Uri>().unwrap();
    if Some("fs") != uri.scheme_str() {
        panic!("filesystem: uri scheme must be uri, got: {:?}", uri.scheme_str());
    }
    let mut relative_file_path = uri.host().unwrap().to_string();
    relative_file_path.push_str(uri.path());
    fs::canonicalize(relative_file_path)
        .await
        .unwrap()
        .to_str()
        .unwrap()
        .to_string()
}

fn make_sandbox_dir_path(home_directory_path: &str, source_app: &str) -> String {
    std::path::Path::new(home_directory_path)
        .join(source_app)
        .to_str()
        .unwrap()
        .to_string()
}

async fn get_file_bytes_left(file: &mut fs::File) -> Result<u64> {
    let current_pos = file.stream_position().await?;
    let metadata = file.metadata().await?;

    Ok(metadata.len() - current_pos)
}

pub async fn fs_sender(
    our_name: &str,
    home_directory_path: &str,
    send_to_loop: MessageSender,
    print_tx: PrintSender,
    mut recv_in_fs: MessageReceiver
) {
    if let Err(e) = create_dir_if_dne(home_directory_path).await {
        panic!(
            "filesystem: failed to create home directory at path {}: {}",
            home_directory_path,
            e
        );
    }
    let home_directory_path = fs::canonicalize(home_directory_path)
        .await
        .unwrap();
    let home_directory_path = home_directory_path
        .to_str()
        .unwrap();

    let mut process_to_open_files: HashMap<String, Arc<Mutex<HashMap<FileRef, fs::File>>>> =
        HashMap::new();
            //Arc::new(RwLock::new(HashMap::new()));

    // //  TODO: store or back up in DB/kv?
    while let Some(message_stack) = recv_in_fs.recv().await {
        let stack_len = message_stack.len();
        let source_ship = &message_stack[stack_len - 1].wire.source_ship;
        let source_app = &message_stack[stack_len - 1].wire.source_app;
        if our_name != source_ship {
            println!(
                "filesystem: request must come from our_name={}, got: {}",
                our_name,
                &message_stack[stack_len - 1],
            );
            continue;
        }
        let open_files = Arc::clone(
            match process_to_open_files.get(source_app) {
                Some(open_files) => open_files,
                None => {
                    //  create process sandbox directory
                    if !HAS_FULL_HOME_ACCESS.contains(source_app) {
                        let sandbox_dir_path = make_sandbox_dir_path(
                            home_directory_path,
                            source_app,
                        );
                        if let Err(e) = create_dir_if_dne(&sandbox_dir_path).await {
                            panic!(
                                "filesystem: failed to create process sandbox directory at path {}: {}",
                                sandbox_dir_path,
                                e,
                            );
                        }
                    }

                    //  create open_files entry
                    process_to_open_files.insert(
                        source_app.to_string(),
                        Arc::new(Mutex::new(HashMap::new())),
                    );
                    process_to_open_files.get(source_app).unwrap()
                },
            }
        );
        tokio::spawn(
            handle_request(
                our_name.to_string(),
                home_directory_path.to_string(),
                message_stack,
                open_files,
                // Arc::clone(&read_access_by_node),
                send_to_loop.clone(),
                print_tx.clone()
            )
        );
    }
}

//  TODO: error handling: should probably send error messages to caller
async fn handle_request(
    our_name: String,
    home_directory_path: String,
    mut messages: MessageStack,
    open_files: Arc<Mutex<HashMap<FileRef, fs::File>>>,
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

    let source_app = &message.wire.source_app;
    let file_path = get_file_path(&request.uri_string).await;
    if HAS_FULL_HOME_ACCESS.contains(source_app) {
        if !std::path::Path::new(&file_path).starts_with(&home_directory_path) {
            panic!(
                "filesystem: {} can only read files within home dir {}; not {}",
                source_app,
                home_directory_path,
                file_path,
            );
        }
    } else {
        let sandbox_dir_path = make_sandbox_dir_path(
            &home_directory_path,
            source_app,
        );
        if !std::path::Path::new(&file_path).starts_with(&sandbox_dir_path) {
            panic!(
                "filesystem: {} can only read files within process sandbox dir {}; not {}",
                source_app,
                sandbox_dir_path,
                file_path,
            );
        }
    }

    let response_payload = match request.action {
        FileSystemAction::Read => {
            //  TODO: use read_exact()?
            let file_contents = fs::read(&file_path).await?;
            let _ = print_tx.send(
                format!(
                    "filesystem: got file at {} of size {}",
                    file_path,
                    file_contents.len()
                    )
            ).await;

            Payload {
                json: Some(json![{"uri_string": request.uri_string}]),  //  TODO: error propagation to caller
                bytes: Some(file_contents),
            }
        },
        FileSystemAction::Write => {
            let Some(payload_bytes) = message.payload.bytes.clone() else {
                panic!("filesystem: received Write without any bytes to append");  //  TODO: change to an error response once responses are real
            };

            fs::write(&file_path, &payload_bytes).await?;

            Payload {
                json: Some(json![{"uri_string": request.uri_string}]),  //  TODO: error propagation to caller
                bytes: None,
            }
        },
        FileSystemAction::OpenRead => {
            //  TODO: refactor shared code with OpenWrite
            let file_ref = FileRef {
                path: file_path.clone(),
                mode: FileMode::Read,
            };
            {
                let open_files_lock = open_files.lock().await;
                if open_files_lock.contains_key(&file_ref) {
                    panic!("filesystem: already have {} open for Read", file_path);
                }
            }

            let file = fs::OpenOptions::new()
                .read(true)
                .open(&file_path)
                .await;

            match file {
                Ok(file) => {
                    {
                        let mut open_files_lock = open_files.lock().await;
                        open_files_lock.insert(file_ref, file);
                    }

                    Payload {
                        json: Some(json![{"uri_string": request.uri_string}]),  //  TODO: error propagation to caller
                        bytes: None,
                    }
                },
                Err(e) => {
                    panic!("filesystem: failed to open file {} for Read: {}", file_path, e);
                },
            }
        },
        FileSystemAction::OpenWrite => {
            //  TODO: refactor shared code with OpenRead
            let file_ref = FileRef {
                path: file_path.clone(),
                mode: FileMode::Append,
            };
            {
                let open_files_lock = open_files.lock().await;
                if open_files_lock.contains_key(&file_ref) {
                    panic!("filesystem: already have {} open for Append", file_path);
                }
            }

            let file = fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&file_path)
                .await;

            match file {
                Ok(file) => {
                    {
                        let mut open_files_lock = open_files.lock().await;
                        open_files_lock.insert(file_ref, file);
                    }

                    Payload {
                        json: Some(json![{"uri_string": request.uri_string}]),  //  TODO: error propagation to caller
                        bytes: None,
                    }
                },
                Err(e) => {
                    panic!("filesystem: failed to open file {} for Append: {}", file_path, e);
                },
            }
        },
        FileSystemAction::Append => {
            let file_ref = FileRef {
                path: file_path.clone(),
                mode: FileMode::Append,
            };
            let mut open_files_lock = open_files.lock().await;
            let file = open_files_lock
                .get_mut(&file_ref)
                .expect(format!(
                    "filesystem: didn't find open file for Append: {}",
                    file_path,
                ).as_str());
            let payload_bytes = message
                .payload
                .bytes
                .clone()
                .expect("filesystem: received Append without any bytes to append");

            file.write_all_buf(&mut Bytes::from(payload_bytes))
                .await
                .expect(
                    format!("filesystem: failed to write all bytes to {}", file_path).as_str()
                );

            Payload {
                json: Some(json![{"uri_string": request.uri_string}]),  //  TODO: error propagation to caller
                bytes: None,
            }
        },
        FileSystemAction::ComputeHash => {
            //  TODO: use read_exact()?
            let file_contents = fs::read(&file_path).await?;

            let mut hasher = Sha256::new();
            hasher.update(&file_contents);
            let hash = hasher.finalize();
            //  truncate
            let hash = u64::from_be_bytes([
                hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7],
            ]);

            let _ = print_tx.send(
                format!(
                    "filesystem: got file at {} of size {} with hash {:x}",
                    file_path,
                    file_contents.len(),
                    hash,
                    )
            ).await;

            Payload {
                json: Some(json![{"uri_string": request.uri_string, "hash": hash}]),  //  TODO: error propagation to caller
                bytes: Some(file_contents),
            }
        },
        FileSystemAction::ReadChunkFromOpen(number_bytes) => {
            let file_ref = FileRef {
                path: file_path.clone(),
                mode: FileMode::Read,
            };
            let mut open_files_lock = open_files.lock().await;
            let file = open_files_lock
                .get_mut(&file_ref)
                .expect(format!(
                    "filesystem: didn't find open file for ReadChunkFromOpen: {}",
                    file_path,
                ).as_str());

            let number_bytes_left = get_file_bytes_left(file)
                .await
                .expect("filesystem: couldn't determine number of bytes left in file");

            let number_bytes_to_read =
                if number_bytes_left < number_bytes {
                    number_bytes_left
                } else {
                    number_bytes
                } as usize;

            let mut bytes: Vec<u8> = vec![0; number_bytes_to_read];

            file.read_exact(&mut bytes)
                .await
                .expect(
                    format!("filesystem: failed to read bytes from {}", file_path).as_str()
                );

            Payload {
                json: Some(json![{"uri_string": request.uri_string}]),  //  TODO: error propagation to caller
                bytes: Some(bytes),
            }
        },
        FileSystemAction::SeekWithinOpen(seek_from) => {
            let file_ref = FileRef {
                path: file_path.clone(),
                mode: FileMode::Read,
            };
            let mut open_files_lock = open_files.lock().await;
            let file = open_files_lock
                .get_mut(&file_ref)
                .expect(format!(
                    "filesystem: didn't find open file for ReadChunkFromOpen: {}",
                    file_path,
                ).as_str());

            if let Err(e) = match seek_from {
                FileSystemSeekFrom::Start(delta) => {
                    file.seek(SeekFrom::Start(delta)).await
                },
                FileSystemSeekFrom::End(delta) => {
                    file.seek(SeekFrom::End(delta)).await
                },
                FileSystemSeekFrom::Current(delta) => {
                    file.seek(SeekFrom::Current(delta)).await
                },
            } {
                panic!("filesystem: failed to seek in {}: {}", file_path, e);
            }

            Payload {
                json: Some(json![{"uri_string": request.uri_string}]),  //  TODO: error propagation to caller
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
