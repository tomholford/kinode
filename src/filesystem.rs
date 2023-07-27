use bytes::Bytes;
use http::Uri;
use sha2::Digest;
use sha2::Sha256;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::sync::Mutex;

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

async fn create_dir_if_dne(path: &str) -> Result<(), FileSystemError> {
    if let Err(_) = fs::read_dir(&path).await {
        match fs::create_dir_all(&path).await {
            Ok(_) => Ok(()),
            Err(e) => Err(FileSystemError::CouldNotMakeDir {
                path: path.into(),
                error: format!("{}", e),
            }),
        }
    } else {
        Ok(())
    }
}

async fn to_absolute_path(
    home_directory_path: &str,
    source_app: &str,
    uri_string: &str
) -> Result<String, FileSystemError> {
    let uri = match uri_string.parse::<Uri>() {
        Ok(uri) => uri,
        Err(_) => return Err(FileSystemError::BadUri {
            uri: uri_string.into(),
            bad_part_name: "entire".into(),
            bad_part: Some(uri_string.into()),
        })
    };

    if Some("fs") != uri.scheme_str() {
        return Err(FileSystemError::BadUri {
            uri: uri_string.into(),
            bad_part_name: "scheme".into(),
            bad_part: match uri.scheme_str() {
                Some(s) => Some(s.into()),
                None => None,
            },
        })
    }
    let mut relative_file_path = uri
        .host()
        .ok_or(FileSystemError::BadUri {
            uri: uri_string.into(),
            bad_part_name: "host".into(),
            bad_part: match uri.host() {
                Some(s) => Some(s.into()),
                None => None,
            },
        })?
        .to_string();
    if "/" != uri.path() {
        relative_file_path.push_str(uri.path());
    }

    let base_path =
        if HAS_FULL_HOME_ACCESS.contains(source_app) {
            home_directory_path.to_string()
        } else {
            join_paths(home_directory_path.into(), source_app.into())?
            // make_sandbox_dir_path(home_directory_path, source_app)
        };

    join_paths(base_path, relative_file_path)
}

fn join_paths(base_path: String, relative_path: String) -> Result<String, FileSystemError> {
    match std::path::Path::new(&base_path)
            .join(&relative_path)
            .to_str()
            .ok_or(FileSystemError::BadPathJoin { base_path, addend: relative_path }) {
        Ok(s) => Ok(s.into()),
        Err(e) => Err(e),
    }
}

async fn get_file_bytes_left(file_path: &str, file: &mut fs::File) -> Result<u64, FileSystemError> {
    let current_pos = match file.stream_position().await {
        Ok(p) => p,
        Err(e) => {
            return Err(FileSystemError::FsError {
                what: "reading current stream position".into(),
                path: file_path.into(),
                error: format!("{}", e),
            })
        },
    };
    let metadata = match file.metadata().await {
        Ok(m) => m,
        Err(e) => {
            return Err(FileSystemError::FsError {
                what: "reading metadata".into(),
                path: file_path.into(),
                error: format!("{}", e),
            })
        },
    };

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

fn make_error_message(
    our_name: String,
    id: u64,
    source_process: String,
    error: FileSystemError,
) -> WrappedMessage {
    WrappedMessage {
        id,
        rsvp: None,
        message: Message {
            message_type: MessageType::Response,
            wire: Wire {
                source_ship: our_name.clone(),
                source_app: "filesystem".into(),
                target_ship: our_name,
                target_app: source_process,
            },
            payload: Payload {
                json: Some(serde_json::to_value(FileSystemResponse::Error(error)).unwrap()),
                bytes: None,
            },
        },
    }
}

pub async fn fs_sender(
    our_name: &str,
    home_directory_path: &str,
    send_to_loop: MessageSender,
    send_to_terminal: PrintSender,
    mut recv_in_fs: MessageReceiver
) {
    if let Err(e) = create_dir_if_dne(home_directory_path).await {
        panic!("{}", e);
    }
    let home_directory_path = fs::canonicalize(home_directory_path)
        .await
        .unwrap();
    let home_directory_path = home_directory_path
        .to_str()
        .unwrap();

    let mut process_to_open_files: HashMap<String, Arc<Mutex<HashMap<FileRef, fs::File>>>> =
        HashMap::new();

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
                        let sandbox_dir_path_result = join_paths(
                            home_directory_path.into(),
                            source_app.into(),
                        );
                        let sandbox_dir_path = match sandbox_dir_path_result {
                            Ok(sandbox_dir_path) => sandbox_dir_path,
                            Err(e) => {
                                send_to_loop
                                    .send(
                                        make_error_message(
                                            our_name.into(),
                                            message.id.clone(),
                                            message.message.wire.source_app.clone(),
                                            e,
                                        )
                                    )
                                    .await
                                    .unwrap();
                                continue;
                            },
                        };
                        if let Err(e) = create_dir_if_dne(&sandbox_dir_path).await {
                            //  TODO: should the error to the requester be a panic?
                            send_to_loop
                                .send(
                                    make_error_message(
                                        our_name.into(),
                                        message.id.clone(),
                                        message.message.wire.source_app.clone(),
                                        e,
                                    )
                                )
                                .await
                                .unwrap();
                            continue;
                            // panic!(
                            //     "filesystem: failed to create process sandbox directory at path {}: {}",
                            //     sandbox_dir_path,
                            //     e,
                            // );
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
        let our_name = our_name.to_string();
        let home_directory_path = home_directory_path.to_string();
        let source_app = source_app.to_string();
        let id = message.id.clone();
        let send_to_loop = send_to_loop.clone();
        let send_to_terminal = send_to_terminal.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_request(
                our_name.clone(),
                home_directory_path,
                message,
                open_files,
                send_to_loop.clone(),
                send_to_terminal,
            ).await {
                send_to_loop
                    .send(
                        make_error_message(
                            our_name.into(),
                            id,
                            source_app,
                            e,
                        )
                    )
                    .await
                    .unwrap();
            }
        });
    }
}

//  TODO: error handling: send error messages to caller
async fn handle_request(
    our_name: String,
    home_directory_path: String,
    message: WrappedMessage,
    open_files: Arc<Mutex<HashMap<FileRef, fs::File>>>,
    send_to_loop: MessageSender,
    send_to_terminal: PrintSender,
) -> Result<(), FileSystemError> {
    let WrappedMessage { id, rsvp, message } = message;
    let Some(value) = message.payload.json.clone() else {
        return Err(FileSystemError::BadJson {
            json: message.payload.json,
            error: "missing payload".into(),
        })
    };

    let request: FileSystemRequest = match serde_json::from_value(value) {
        Ok(r) => r,
        Err(e) => {
            return Err(FileSystemError::BadJson {
                json: message.payload.json,
                error: format!("parse failed: {:?}", e),
            })
        },
    };

    let source_app = &message.wire.source_app;
    // let file_path = get_file_path(&request.uri_string).await;
    let file_path = to_absolute_path(
        &home_directory_path,
        source_app,
        &request.uri_string
    ).await?;
    if HAS_FULL_HOME_ACCESS.contains(source_app) {
        if !std::path::Path::new(&file_path).starts_with(&home_directory_path) {
            return Err(FileSystemError::IllegalAccess {
                process_name: source_app.into(),
                attempted_dir: file_path,
                sandbox_dir: home_directory_path,
            })
        }
    } else {
        let sandbox_dir_path = join_paths(
            home_directory_path,
            source_app.into(),
        )?;
        if !std::path::Path::new(&file_path).starts_with(&sandbox_dir_path) {
            return Err(FileSystemError::IllegalAccess {
                process_name: source_app.into(),
                attempted_dir: file_path,
                sandbox_dir: sandbox_dir_path,
            })
        }
    }

    let response_payload = match request.action {
        FileSystemAction::Read => {
            //  TODO: use read_exact()?
            let file_contents = match fs::read(&file_path).await {
                Ok(fc) => fc,
                Err(e) => {
                    return Err(FileSystemError::ReadFailed {
                        path: file_path,
                        error: format!("{}", e),
                    })
                },
            };
            let hash = compute_truncated_hash(&file_contents);
            let _ = send_to_terminal.send(
                Printout {
                    verbosity: 0,
                    content: format!(
                        "filesystem: got file at {} of size {} with hash {}",
                        file_path,
                        file_contents.len(),
                        hash,
                    )
                }
            ).await;

            Payload {
                json: Some(
                    serde_json::to_value(
                        FileSystemResponse::Read(FileSystemUriHash {
                            uri_string: request.uri_string,
                            hash,
                        })
                    ).unwrap()
                ),
                bytes: Some(file_contents),
            }
        },
        FileSystemAction::Write => {
            let Some(payload_bytes) = message.payload.bytes.clone() else {
                return Err(FileSystemError::BadBytes { action: "Write".into() })
            };
            if let Err(e) = fs::write(&file_path, &payload_bytes).await {
                return Err(FileSystemError::WriteFailed {
                    path: file_path,
                    error: format!("{}", e),
                })
            };

            Payload {
                json: Some(
                    serde_json::to_value(FileSystemResponse::Write(request.uri_string))
                        .unwrap()
                ),
                bytes: None,
            }
        },
        FileSystemAction::GetMetadata => {
            //  TODO: use read_exact()?
            let mut file = match fs::OpenOptions::new()
                    .read(true)
                    .open(&file_path)
                    .await {
                Ok(f) => f,
                Err(e) => {
                    return Err(FileSystemError::OpenFailed {
                        path: file_path,
                        mode: "OneOffRead".into(),
                        error: format!("{}", e),
                    })
                },
            };
            let metadata = match file.metadata().await {
                Ok(m) => m,
                Err(e) => {
                    return Err(FileSystemError::FsError {
                        what: "reading metadata".into(),
                        path: file_path,
                        error: format!("{}", e),
                    })
                },
            };
            let mut file_contents = vec![];
            if let Err(e) = file.read_to_end(&mut file_contents).await {
                return Err(FileSystemError::ReadFailed {
                    path: file_path,
                    error: format!("{}", e),
                })
            }

            let hash = compute_truncated_hash(&file_contents);

            Payload {
                json: Some(
                    serde_json::to_value(
                        FileSystemResponse::GetMetadata(FileSystemMetadata {
                            uri_string: request.uri_string,
                            hash,
                            is_dir: metadata.is_dir(),
                            is_file: metadata.is_file(),
                            is_symlink: metadata.is_symlink(),
                            len: metadata.len(),
                        })
                    ).unwrap()
                ),
                bytes: None,
            }
        },
        FileSystemAction::OpenRead => {
            let file_ref = FileRef {
                path: file_path.clone(),
                mode: FileMode::Read,
            };
            {
                let open_files_lock = open_files.lock().await;
                if open_files_lock.contains_key(&file_ref) {
                    return Err(FileSystemError::AlreadyOpen {
                        path: file_path,
                        mode: "Read".into(),
                    })
                }
            }

            let file_result = fs::OpenOptions::new()
                .read(true)
                .open(&file_path)
                .await;

            match file_result {
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
                        ),
                        bytes: None,
                    }
                },
                Err(e) => {
                    return Err(FileSystemError::OpenFailed {
                        path: file_path,
                        mode: "Read".into(),
                        error: format!("{}", e),
                    })
                },
            }
        },
        FileSystemAction::OpenAppend => {
            let file_ref = FileRef {
                path: file_path.clone(),
                mode: FileMode::Append,
            };
            {
                let open_files_lock = open_files.lock().await;
                if open_files_lock.contains_key(&file_ref) {
                    return Err(FileSystemError::AlreadyOpen {
                        path: file_path,
                        mode: "Append".into(),
                    })
                }
            }

            let file_result = fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&file_path)
                .await;

            match file_result {
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
                        ),
                        bytes: None,
                    }
                },
                Err(e) => {
                    return Err(FileSystemError::OpenFailed {
                        path: file_path,
                        mode: "Append".into(),
                        error: format!("{}", e),
                    })
                },
            }
        },
        FileSystemAction::Append => {
            let file_ref = FileRef {
                path: file_path.clone(),
                mode: FileMode::Append,
            };
            let mut open_files_lock = open_files.lock().await;
            let file = match open_files_lock
                    .get_mut(&file_ref) {
                Some(f) => f,
                None => {
                    return Err(FileSystemError::NotCurrentlyOpen {
                        path: file_path,
                        mode: "Append".into(),
                    })
                },
            };
            let payload_bytes = match message
                    .payload
                    .bytes {
                Some(b) => b.clone(),
                None =>  {
                    return Err(FileSystemError::BadBytes { action: "Append".into() })
                }
            };
            if let Err(e) = file.write_all_buf(&mut Bytes::from(payload_bytes)).await {
                return Err(FileSystemError::WriteFailed {
                    path: file_path,
                    error: format!("{}", e),
                })
            }

            Payload {
                json: Some(
                    serde_json::to_value(FileSystemResponse::Append(request.uri_string))
                        .unwrap()
                ),
                bytes: None,
            }
        },
        FileSystemAction::ReadChunkFromOpen(number_bytes) => {
            let file_ref = FileRef {
                path: file_path.clone(),
                mode: FileMode::Read,
            };
            let mut open_files_lock = open_files.lock().await;
            let file = match open_files_lock
                    .get_mut(&file_ref) {
                Some(f) => f,
                None => {
                    return Err(FileSystemError::NotCurrentlyOpen {
                        path: file_path,
                        mode: "ReadChunkFromOpen".into(),
                    })
                },
            };

            let number_bytes_left = get_file_bytes_left(&file_path, file).await?;

            let number_bytes_to_read =
                if number_bytes_left < number_bytes {
                    number_bytes_left
                } else {
                    number_bytes
                } as usize;

            let mut file_contents: Vec<u8> = vec![0; number_bytes_to_read];

            if let Err(e) = file.read_exact(&mut file_contents).await {
                return Err(FileSystemError::ReadFailed {
                    path: file_path,
                    error: format!("{}", e),
                })
            }

            Payload {
                json: Some(
                    serde_json::to_value(
                        FileSystemResponse::ReadChunkFromOpen(FileSystemUriHash {
                            uri_string: request.uri_string,
                            hash: compute_truncated_hash(&file_contents),
                        })
                    )
                        .unwrap()
                ),
                bytes: Some(file_contents),
            }
        },
        FileSystemAction::SeekWithinOpen(seek_from) => {
            let file_ref = FileRef {
                path: file_path.clone(),
                mode: FileMode::Read,
            };
            let mut open_files_lock = open_files.lock().await;

            let file = match open_files_lock
                    .get_mut(&file_ref) {
                Some(f) => f,
                None => {
                    return Err(FileSystemError::NotCurrentlyOpen {
                        path: file_path,
                        mode: "SeekWithinOpen".into(),
                    })
                },
            };

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
                return Err(FileSystemError::FsError {
                    what: "seeking".into(),
                    path: file_path,
                    error: format!("{}", e),
                })
            }

            Payload {
                json: Some(
                    serde_json::to_value(FileSystemResponse::SeekWithinOpen(request.uri_string))
                        .unwrap()
                ),
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
