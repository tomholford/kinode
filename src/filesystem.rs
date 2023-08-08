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

const HASH_READER_CHUNK_SIZE: usize = 1_024;

#[derive(Eq, Hash, PartialEq)]
struct FileRef {
    path: String,
    mode: FileSystemMode,
}

fn get_entry_type(_is_dir: bool, is_file: bool, is_symlink: bool) -> FileSystemEntryType {
    if is_symlink {
        FileSystemEntryType::Symlink
    } else if is_file {
        FileSystemEntryType::File
    } else {
        FileSystemEntryType::Dir
    }
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

async fn compute_truncated_hash_reader(file_path: &str, mut file: fs::File) -> Result<u64, FileSystemError> {
    let mut hasher = Sha256::new();
    let mut buffer = [0; HASH_READER_CHUNK_SIZE];  //  1kiB
    let number_bytes_left = get_file_bytes_left(&file_path, &mut file).await? as usize;
    let mut number_iterations = number_bytes_left / HASH_READER_CHUNK_SIZE;
    let number_bytes_left_after_loop =
        number_bytes_left - number_iterations * HASH_READER_CHUNK_SIZE;

    while number_iterations > 0 {
        let count = match file.read_exact(&mut buffer).await {
            Ok(c) => c,
            Err(e) => {
                return Err(FileSystemError::ReadFailed {
                    path: file_path.into(),
                    error: format!("{}", e),
                })
            },
        };
        hasher.update(&buffer[..count]);

        number_iterations -= 1;
    }

    let mut buffer = vec![0; number_bytes_left_after_loop];
    let count = match file.read_exact(&mut buffer).await {
        Ok(c) => c,
        Err(e) => {
            return Err(FileSystemError::ReadFailed {
                path: file_path.into(),
                error: format!("{}", e),
            })
        },
    };
    hasher.update(&buffer[..count]);

    let hash = hasher.finalize();
    //  truncate
    Ok(u64::from_be_bytes(
        [hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7]]
    ))
}

fn compute_truncated_hash_bytes(file_contents: &Vec<u8>) -> u64 {
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
    our_name: String,
    home_directory_path: String,
    send_to_loop: MessageSender,
    send_to_terminal: PrintSender,
    mut recv_in_fs: MessageReceiver
) {
    if let Err(e) = create_dir_if_dne(&home_directory_path).await {
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
        if &our_name != source_ship {
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
                                            our_name.clone(),
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
                                        our_name.clone(),
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
        let our_name = our_name.clone();
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
            let hash = compute_truncated_hash_bytes(&file_contents);
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
            let file = match fs::OpenOptions::new()
                    .read(true)
                    .open(&file_path)
                    .await {
                Ok(f) => f,
                Err(e) => {
                    return Err(FileSystemError::OpenFailed {
                        path: file_path,
                        mode: FileSystemMode::Read,
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

            let hash = compute_truncated_hash_reader(&file_path, file).await?;

            Payload {
                json: Some(
                    serde_json::to_value(
                        FileSystemResponse::GetMetadata(FileSystemMetadata {
                            uri_string: request.uri_string,
                            hash: Some(hash),
                            entry_type: get_entry_type(
                                metadata.is_dir(),
                                metadata.is_file(),
                                metadata.is_symlink(),
                            ),
                            len: metadata.len(),
                        })
                    ).unwrap()
                ),
                bytes: None,
            }
        },
        FileSystemAction::ReadDir => {
            let mut entries = match fs::read_dir(&file_path).await {
                Ok(es) => es,
                Err(e) => return Err(FileSystemError::ReadFailed {
                    path: file_path,
                    error: format!("{}", e),
                }),
            };

            let mut metadatas: Vec<FileSystemMetadata> = vec![];

            loop {
                let entry = match entries.next_entry().await {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = send_to_terminal.send(Printout {
                            verbosity: 0,
                            content: format!(
                                "filesystem: ReadDir couldn't get next entry: {}",
                                e,
                            ),
                        }).await;
                        continue;
                    },
                };
                let Some(entry) = entry else {
                    break;
                };

                let metadata = match entry.metadata().await {
                    Ok(m) => m,
                    Err(e) => {
                        let _ = send_to_terminal.send(Printout {
                            verbosity: 0,
                            content: format!(
                                "filesystem: ReadDir couldn't read metadata: {}",
                                e,
                            )
                        }).await;
                        continue;
                    },
                };

                let file_name = match entry.file_name().into_string() {
                    Ok(f) => f,
                    Err(e) => {
                        let _ = send_to_terminal.send(Printout {
                            verbosity: 0,
                            content: format!(
                                "filesystem: ReadDir couldn't put entry name into string: {:?}",
                                e,
                            ),
                        }).await;
                        continue;
                    },
                };

                metadatas.push(FileSystemMetadata {
                    uri_string: file_name,
                    hash: None,
                    entry_type: get_entry_type(
                        metadata.is_dir(),
                        metadata.is_file(),
                        metadata.is_symlink()
                    ),
                    len: metadata.len(),
                })
            }

            Payload {
                json: Some(
                    serde_json::to_value(FileSystemResponse::ReadDir(metadatas)).unwrap()
                ),
                bytes: None,
            }
        }
        FileSystemAction::Open(mode) => {
            let file_ref = FileRef {
                path: file_path.clone(),
                mode: match mode.clone() {
                    FileSystemMode::Read => FileSystemMode::Read,
                    FileSystemMode::Append => FileSystemMode::Append,
                    FileSystemMode::AppendOverwrite => FileSystemMode::Append,
                },
            };
            {
                let open_files_lock = open_files.lock().await;
                if open_files_lock.contains_key(&file_ref) {
                    return Err(FileSystemError::AlreadyOpen {
                        path: file_path,
                        mode,
                    })
                }
            }

            let file_result = match mode {
                FileSystemMode::Read => {
                    fs::OpenOptions::new()
                        .read(true)
                        .open(&file_path)
                        .await
                },
                FileSystemMode::Append => {
                    fs::OpenOptions::new()
                        .append(true)
                        .create(true)
                        .open(&file_path)
                        .await
                },
                //  TODO: rename
                FileSystemMode::AppendOverwrite => {
                    fs::OpenOptions::new()
                        // .append(true)
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .open(&file_path)
                        .await
                },
            };

            match file_result {
                Ok(file) => {
                    {
                        let mut open_files_lock = open_files.lock().await;
                        open_files_lock.insert(file_ref, file);
                    }

                    Payload {
                        json: Some(
                            serde_json::to_value(
                                FileSystemResponse::Open {
                                    uri_string: request.uri_string,
                                    mode,
                                }
                            ).unwrap()
                        ),
                        bytes: None,
                    }
                },
                Err(e) => {
                    return Err(FileSystemError::OpenFailed {
                        path: file_path,
                        mode,
                        error: format!("{}", e),
                    })
                },
            }
        },
        FileSystemAction::Close(mode) => {
            let file_ref = FileRef {
                path: file_path.clone(),
                mode: mode.clone(),
            };
            let mut open_files_lock = open_files.lock().await;
            open_files_lock.remove(&file_ref);
            Payload {
                json: Some(
                    serde_json::to_value(
                        FileSystemResponse::Close {
                            uri_string: request.uri_string,
                            mode,
                        }
                    ).unwrap()
                ),
                bytes: None,
            }
        }
        FileSystemAction::Append => {
            let file_ref = FileRef {
                path: file_path.clone(),
                mode: FileSystemMode::Append,
            };
            let mut open_files_lock = open_files.lock().await;
            let file = match open_files_lock
                    .get_mut(&file_ref) {
                Some(f) => f,
                None => {
                    return Err(FileSystemError::NotCurrentlyOpen {
                        path: file_path,
                        mode: FileSystemMode::Append,
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
                mode: FileSystemMode::Read,
            };
            let mut open_files_lock = open_files.lock().await;
            let file = match open_files_lock
                    .get_mut(&file_ref) {
                Some(f) => f,
                None => {
                    return Err(FileSystemError::NotCurrentlyOpen {
                        path: file_path,
                        mode: FileSystemMode::Read,
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
                            hash: compute_truncated_hash_bytes(&file_contents),
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
                mode: FileSystemMode::Read,
            };
            let mut open_files_lock = open_files.lock().await;

            let file = match open_files_lock
                    .get_mut(&file_ref) {
                Some(f) => f,
                None => {
                    return Err(FileSystemError::NotCurrentlyOpen {
                        path: file_path,
                        mode: FileSystemMode::Read,
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
