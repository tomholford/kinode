use anyhow::Result;
use bytes::Bytes;
use http::Uri;
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

// async fn get_file_path(uri_string: &str) -> String {
//     let uri = uri_string.parse::<Uri>().unwrap();
//     if Some("fs") != uri.scheme_str() {
//         panic!("filesystem: uri scheme must be uri, got: {:?}", uri.scheme_str());
//     }
//     let mut relative_file_path = uri.host().unwrap().to_string();
//     relative_file_path.push_str(uri.path());
//     fs::canonicalize(relative_file_path)
//         .await
//         .unwrap()
//         .to_str()
//         .unwrap()
//         .to_string()
// }
// 
// async fn get_file_path_2(base_path: String, uri_string: &str) -> String {
//     let uri = uri_string.parse::<Uri>().unwrap();
//     if Some("fs") != uri.scheme_str() {
//         panic!("filesystem: uri scheme must be uri, got: {:?}", uri.scheme_str());
//     }
//     let mut relative_file_path = uri.host().unwrap().to_string();
//     relative_file_path.push_str(uri.path());
//     std::path::Path::new(&base_path)
//         .join(relative_file_path)
//         .to_str()
//         .unwrap()
//         .to_string()
// }

async fn to_absolute_path(
    home_directory_path: &str,
    source_app: &str,
    uri_string: &str
) -> String {
    let uri = uri_string.parse::<Uri>().unwrap();
    if Some("fs") != uri.scheme_str() {
        panic!("filesystem: uri scheme must be uri, got: {:?}", uri.scheme_str());
    }
    let mut relative_file_path = uri.host().unwrap().to_string();
    if "/" != uri.path() {
        relative_file_path.push_str(uri.path());
    }

    let base_path =
        if HAS_FULL_HOME_ACCESS.contains(source_app) {
            home_directory_path.to_string()
        } else {
            make_sandbox_dir_path(home_directory_path, source_app)
        };

    std::path::Path::new(&base_path)
        .join(relative_file_path)
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

fn compute_truncated_hash(file_contents: &Vec<u8>) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(file_contents);
    let hash = hasher.finalize();
    //  truncate
    u64::from_be_bytes(
        [hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7]]
    )
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

    //  TODO: store or back up in DB/kv?
    while let Some(message) = recv_in_fs.recv().await {
        let source_ship = &message.message.wire.source_ship;
        let source_app = &message.message.wire.source_app;
        if our_name != source_ship {
            println!(
                "filesystem: request must come from our_name={}, got: {}",
                our_name,
                &message,
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
                message,
                open_files,
                // Arc::clone(&read_access_by_node),
                send_to_loop.clone(),
                print_tx.clone()
            )
        );
    }
}

//  TODO: error handling: send error messages to caller
async fn handle_request(
    our_name: String,
    home_directory_path: String,
    message: WrappedMessage,
    open_files: Arc<Mutex<HashMap<FileRef, fs::File>>>,
    send_to_loop: MessageSender,
    print_tx: PrintSender,
) -> Result<(), Error> {
    println!("filesystem: got {}", message);
    let WrappedMessage { id, rsvp, message } = message;
    if "filesystem".to_string() != message.wire.target_app {
        panic!("filesystem: filesystem must be target.app, got: {:?}", message);
    }
    let Some(value) = message.payload.json.clone() else {
        panic!("filesystem: request must have JSON payload, got: {:?}", message);
    };

    let request: FileSystemRequest = match serde_json::from_value(value) {
        Ok(r) => r,
        Err(e) => {
            panic!("filesystem: couldn't parse Request: {:?}, with error: {}", message.payload.json, e)
        },
    };

    let source_app = &message.wire.source_app;
    // let file_path = get_file_path(&request.uri_string).await;
    let file_path = to_absolute_path(
        &home_directory_path,
        source_app,
        &request.uri_string
    ).await;
    println!("filesystem: file_path: {}", file_path);
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
            let hash = compute_truncated_hash(&file_contents);
            let _ = print_tx.send(
                format!(
                    "filesystem: got file at {} of size {} with hash {}",
                    file_path,
                    file_contents.len(),
                    hash,
                )
            ).await;

            Payload {
                json: Some(
                    serde_json::to_value(
                        FileSystemResponse::Read(FileSystemUriHash {
                            uri_string: request.uri_string,
                            hash: hash,
                        })
                    )
                        .unwrap()
                ),  //  TODO: error propagation to caller
                bytes: Some(file_contents),
            }
        },
        FileSystemAction::Write => {
            let Some(payload_bytes) = message.payload.bytes.clone() else {
                panic!("filesystem: received Write without any bytes to append");  //  TODO: change to an error response once responses are real
            };

            fs::write(&file_path, &payload_bytes).await?;

            Payload {
                json: Some(
                    serde_json::to_value(FileSystemResponse::Write(request.uri_string))
                        .unwrap()
                ),  //  TODO: error propagation to caller
                bytes: None,
            }
        },
        FileSystemAction::GetMetadata => {
            //  TODO: use read_exact()?
            // let file_contents = fs::read(&file_path).await?;
            let mut file = fs::OpenOptions::new()
                .read(true)
                .open(&file_path)
                .await?;
            let metadata = file.metadata().await?;
            let mut file_contents = vec![];
            file.read_to_end(&mut file_contents).await?;

            let hash = compute_truncated_hash(&file_contents);

            Payload {
                json: Some(
                    serde_json::to_value(
                        FileSystemResponse::GetMetadata(FileSystemMetadata {
                            uri_string: request.uri_string,
                            hash: hash,
                            is_dir: metadata.is_dir(),
                            is_file: metadata.is_file(),
                            is_symlink: metadata.is_symlink(),
                            len: metadata.len(),
                        })
                    ).unwrap()
                ),  //  TODO: error propagation to caller
                bytes: None,
            }
        },
        FileSystemAction::OpenRead => {
            //  TODO: refactor shared code with OpenAppend
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
                        json: Some(
                            serde_json::to_value(
                                FileSystemResponse::OpenRead(request.uri_string)
                            ).unwrap()
                        ),  //  TODO: error propagation to caller
                        bytes: None,
                    }
                },
                Err(e) => {
                    panic!("filesystem: failed to open file {} for Read: {}", file_path, e);
                },
            }
        },
        FileSystemAction::OpenAppend => {
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
                        json: Some(
                            serde_json::to_value(
                                FileSystemResponse::OpenAppend(request.uri_string)
                            ).unwrap()
                        ),  //  TODO: error propagation to caller
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
                json: Some(
                    serde_json::to_value(FileSystemResponse::Append(request.uri_string))
                        .unwrap()
                ),  //  TODO: error propagation to caller
                bytes: None,
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

            let mut file_contents: Vec<u8> = vec![0; number_bytes_to_read];

            file.read_exact(&mut file_contents)
                .await
                .expect(
                    format!("filesystem: failed to read bytes from {}", file_path).as_str()
                );

            Payload {
                json: Some(
                    serde_json::to_value(
                        FileSystemResponse::ReadChunkFromOpen(FileSystemUriHash {
                            uri_string: request.uri_string,
                            hash: compute_truncated_hash(&file_contents),
                        })
                    )
                        .unwrap()
                ),  //  TODO: error propagation to caller
                bytes: Some(file_contents),
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
                json: Some(
                    serde_json::to_value(FileSystemResponse::SeekWithinOpen(request.uri_string))
                        .unwrap()
                ),  //  TODO: error propagation to caller
                bytes: None,
            }
        },
    };

    let response = WrappedMessage {
        id,
        rsvp,
        message: Message {
            message_type: MessageType::Response,
            wire: Wire {
                source_ship: our_name.clone(),
                source_app: "filesystem".to_string(),
                target_ship: our_name.clone(),
                target_app: message.wire.source_app.clone(),
            },
            payload: response_payload,
        },
    };

    let _ = send_to_loop.send(response).await;

    Ok(())
}
