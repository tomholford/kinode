/// log structured filesystem
/// immutable/append []
use blake3::Hasher;
use hex;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;
use tokio::io::{self, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};
use uuid;

use crate::types::*;
use crate::lfs::manifest::{Manifest, BackupImmutable, BackupAppendable, ManifestRecord, FileType, InMemoryFile};

mod manifest;

// const CHUNK_SIZE: u64 = 256 * 1024; // 256kb


//  INTERFACE

#[derive(Serialize, Deserialize, Debug)]
pub enum FsAction {
    Write,
    Append(Option<[u8; 32]>),
    Read([u8; 32]),
    ReadChunk(ReadChunkRequest),
    PmWrite,                     //  specific case for process manager persistance.
    Delete([u8; 32]),
    Length([u8; 32]),
}

#[derive(Serialize, Deserialize, Debug)]
pub enum FsResponse {
    //  bytes are in payload_bytes
    Read([u8; 32]),
    ReadChunk([u8; 32]),
    Write([u8; 32]),
    Append([u8; 32]),
    Delete([u8; 32]),
    Length(u64),
    //  use FileSystemError
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ReadChunkRequest {
    file_hash: [u8; 32],
    start: u64,
    length: u64,
}

// special process manager uuid
const PM_UUID: u128 = 0;

pub async fn fs_sender(
    our_name: String,
    home_directory_path: String,
    send_to_loop: MessageSender,
    send_to_terminal: PrintSender,
    mut recv_in_fs: MessageReceiver,
) {
    // fs bootstrapping, create home_directory, fs directory inside it, manifest + log if none.
    if let Err(e) = create_dir_if_dne(&home_directory_path).await {
        panic!("{}", e);
    }
    let lfs_directory_path_str = format!("{}/lfs", &home_directory_path);

    if let Err(e) = create_dir_if_dne(&lfs_directory_path_str).await {
        panic!("{}", e);
    }
    let lfs_directory_path: std::path::PathBuf =
        fs::canonicalize(lfs_directory_path_str).await.unwrap();

    //  open and load manifest+log

    let manifest_path = lfs_directory_path.join("manifest.bin");

    let mut manifest_file = fs::OpenOptions::new()
        .append(true)
        .read(true)
        .create(true)
        .open(&manifest_path)
        .await
        .expect("fs: failed to open manifest file");

    let _wal_path = lfs_directory_path.join("wal.bin");

    // let mut wal_file = fs::OpenOptions::new()
    //     .append(true)
    //     .read(true)
    //     .create(true)
    //     .open(&wal_path)
    //     .await
    //     .expect("fs: failed to open WAL file");

    //  in memory details about files.
    let manifest = Manifest::load(manifest_file, &lfs_directory_path).await.expect("manifest load failed!");

    //  println!("whole manifest {:?}", manifest);
    //  let wal_file = Arc::new(RwLock::new(wal_file));

    //  into main loop
    while let Some(wrapped_message) = recv_in_fs.recv().await {
        let WrappedMessage { ref id, target: _, rsvp: _, message: Ok(Message { ref source, content: _ }), }
                = wrapped_message else {
            println!(
                "lfs: got weird message from {}: {}",
                our_name, &wrapped_message,
            );
            continue;
        };

        let source_process = &source.process;
        if our_name != source.node {
            println!(
                "lfs: request must come from our_name={}, got: {}",
                our_name, &wrapped_message,
            );
            continue;
        }

        //  internal structures have Arc::clone setup.
        let manifest_clone = manifest.clone();

        // let wal_file_clone = wal_file.clone();
        let lfs_directory_path_clone = lfs_directory_path.clone();

        let our_name = our_name.clone();
        let source_process = source_process.into();
        let id = id.clone();
        let send_to_loop = send_to_loop.clone();
        let send_to_terminal = send_to_terminal.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_request(
                our_name.clone(),
                wrapped_message,
                lfs_directory_path_clone,
                manifest_clone,
                send_to_loop.clone(),
                send_to_terminal,
            )
            .await
            {
                send_to_loop
                    .send(make_error_message(our_name.into(), id, source_process, e))
                    .await
                    .unwrap();
            }
        });
    }
}

async fn handle_request(
    our_name: String,
    wrapped_message: WrappedMessage,
    lfs_directory_path: PathBuf,
    manifest: Manifest,
    send_to_loop: MessageSender,
    _send_to_terminal: PrintSender,
) -> Result<(), FileSystemError> {
    let WrappedMessage { id, target: _, rsvp, message: Ok(Message { source, content }), }
            = wrapped_message else {
        return Err(FileSystemError::LFSError { error: "got weird message".to_string() });
    };
    let Some(value) = content.payload.json.clone() else {
        return Err(FileSystemError::BadJson {
            json: content.payload.json,
            error: "missing payload".into(),
        })
    };

    // println!("got message: {:?}", value);

    let MessageType::Request(is_expecting_response) = content.message_type else {
        return Err(FileSystemError::BadJson {
            json: content.payload.json,
            error: "not a Request".into(),
        })
    };

    let action: FsAction = match serde_json::from_value(value) {
        Ok(r) => r,
        Err(e) => {
            return Err(FileSystemError::BadJson {
                json: content.payload.json,
                error: format!("parse failed: {:?}", e),
            })
        }
    };

    let response_payload = match action {
        FsAction::Write=> {
            let Some(data) = content.payload.bytes.content.clone() else {
                return Err(FileSystemError::BadBytes { action: "Write".into() })
            };

            let file_uuid = uuid::Uuid::new_v4().as_u128();
            let file_length = data.len() as u64;

            let mut hasher = blake3::Hasher::new();
            hasher.update(&data);
            let file_hash: [u8; 32] = hasher.finalize().into();

            //  create and write underlying
            let write_result = write_immutable(file_hash, &data, &lfs_directory_path).await;
            if let Err(e) = write_result {
                return Err(FileSystemError::LFSError {
                    error: format!("write failed: {}", e),
                });
            } else {
                //  append to manifest
                let backup = BackupImmutable {
                    file_uuid,
                    file_hash,
                    file_length,
                    backup: Vec::new(),
                };

                let record = ManifestRecord::BackupI(backup);


                let _ = manifest.add_immutable(&record).await?;
            }

            Payload {
                json: Some(serde_json::to_value(FsResponse::Write(file_hash)).unwrap()),
                bytes: PayloadBytes {
                    circumvent: Circumvent::False,
                    content: None,
                },
            }
        }
        FsAction::Read(file_hash) => {
            // obtain read locks.
            match manifest.get_by_hash(&file_hash).await {
                None => return Err(FileSystemError::LFSError {
                    error: format!("no file found for hash: {:?}", file_hash),
                }),
                Some((file, uuid)) => {
                    let data = match file.file_type {
                        FileType::Appendable => {
                            read_appendable(uuid, &lfs_directory_path, None, None).await?
                        }
                        FileType::Immutable => {
                            read_immutable(&file_hash, &lfs_directory_path, None, None).await?
                        }
                    };

                    Payload {
                        json: Some(serde_json::to_value(FsResponse::Read(file_hash)).unwrap()),
                        bytes: PayloadBytes {
                            circumvent: Circumvent::False,
                            content: Some(data),
                        },
                    }
                }
            }
        },
        FsAction::ReadChunk(req) => {
            match manifest.get_by_hash(&req.file_hash).await {
                None => return Err(FileSystemError::LFSError {
                    error: format!("no file found for hash: {:?}", req.file_hash),
                }),
                Some((file, uuid)) => {
                    let data = match file.file_type {
                        FileType::Appendable => {
                            read_appendable(uuid, &lfs_directory_path, Some(req.start), Some(req.length)).await?
                        },
                        FileType::Immutable => {
                            read_immutable(&req.file_hash, &lfs_directory_path, Some(req.start), Some(req.length)).await?
                        },
                    };
        
                    Payload {
                        json: Some(serde_json::to_value(FsResponse::Read(req.file_hash)).unwrap()),
                        bytes: PayloadBytes {
                            circumvent: Circumvent::False,
                            content: Some(data),
                        },
                    }
                }
            }
        },
        // specific process manager write:
        FsAction::PmWrite => {
            if "process_manager" != source.process {
                return Err(FileSystemError::LFSError {
                    error: "Only process_manager can write to PmWrite".into(),
                });
            }

            let Some(data) = content.payload.bytes.content.clone() else {
                return Err(FileSystemError::BadBytes { action: "PmWrite".into() })
            };

            let file_uuid = PM_UUID;

            //  TODO old hash deletes.

            //  create new immutable file
            let mut hasher = blake3::Hasher::new();
            hasher.update(&data);
            let file_hash: [u8; 32] = hasher.finalize().into();

           
            //  write underlying
            let write_result = write_immutable(file_hash, &data, &lfs_directory_path).await;
            if let Err(e) = write_result {
                return Err(FileSystemError::LFSError {
                    error: format!("write failed: {}", e),
                });
            } else {
                //  append to manifest
                let backup = BackupImmutable {
                    file_uuid,
                    file_hash,
                    file_length: data.len() as u64,
                    backup: Vec::new(),
                };

                let record = ManifestRecord::BackupI(backup);

                let _ = manifest.add_immutable(&record).await?;
            }

            Payload {
                json: Some(serde_json::to_value(FsResponse::Write(file_hash)).unwrap()),
                bytes: PayloadBytes {
                    circumvent: Circumvent::False,
                    content: None,
                },
            }
        },
        FsAction::Delete(del) => {
            // todo command delete specifics.

            Payload {
                json: Some(serde_json::to_value(FsResponse::Delete(del)).unwrap()),
                bytes: PayloadBytes {
                    circumvent: Circumvent::False,
                    content: None,
                },
            }
        }, FsAction::Append(maybe_file_hash) => {
            let Some(data) = content.payload.bytes.content.clone() else {
                return Err(FileSystemError::BadBytes { action: "Write".into() })
            };


            let (file, uuid) = if let Some(existing_file_hash) = maybe_file_hash {
                match manifest.get_by_hash(&existing_file_hash).await {
                    Some((file, uuid)) => {
                        (file, uuid)
                    },
                    None => return Err(FileSystemError::LFSError {
                        error: format!("no file found for hash: {:?}", existing_file_hash),
                    }),
                }
            } else {
                let new_file = InMemoryFile {
                    hash: [0; 32], // Placeholder, will be updated later
                    file_type: FileType::Appendable,
                    hasher: Hasher::new(),
                    file_length: 0,
                };
                let uuid = uuid::Uuid::new_v4().as_u128();
                (new_file, uuid)
            };
        
            let chunk_length = data.len() as u64;
            let mut temp_hasher = file.hasher.clone();
            temp_hasher.update(&data);
            let new_hash: [u8; 32] = temp_hasher.finalize().into();
        
            let write_result = write_appendable(uuid, &data, &lfs_directory_path).await;
            if let Err(e) = write_result {
                return Err(FileSystemError::LFSError {
                    error: format!("write failed: {}", e),
                });
            }
        
            let backup = BackupAppendable {
                file_uuid: uuid,
                file_hash: new_hash,
                file_length: file.file_length + chunk_length,
                backup: Vec::new(),
            };
        
            let record = ManifestRecord::BackupA(backup);
        
            let _ = manifest.add_append(&record).await?;
        
            let _ = manifest.insert(uuid,InMemoryFile {
                    hash: new_hash,
                    file_type: FileType::Appendable,
                    hasher: temp_hasher,
                    file_length: file.file_length + chunk_length,
                },
            ).await?;
            
            //  println!("appended to new_hash! {:?}", new_hash);

            Payload {
                json: Some(serde_json::to_value(FsResponse::Append(new_hash)).unwrap()),
                bytes: PayloadBytes {
                    circumvent: Circumvent::False,
                    content: None,
                },
            }
        }, FsAction::Length(file_hash) => {
            match manifest.get_by_hash(&file_hash).await {
                None => return Err(FileSystemError::LFSError {
                    error: format!("no file found for hash: {:?}", file_hash),
                }),
                Some((file, _uuid)) => {
                    Payload {
                        json: Some(serde_json::to_value(FsResponse::Length(file.file_length)).unwrap()),
                        bytes: PayloadBytes {
                            circumvent: Circumvent::False,
                            content: None,
                        },
                    }
                }
            }
        },
    };

    if is_expecting_response {
        let response = WrappedMessage {
            id,
            target: ProcessNode {
                node: our_name.clone(),
                process: source.process.clone(),
            },
            rsvp,
            message: Ok(Message {
                source: ProcessNode {
                    node: our_name.clone(),
                    process: "lfs".into(),
                },
                content: MessageContent {
                    message_type: MessageType::Response,
                    payload: response_payload,
                },
            }),
        };

        let _ = send_to_loop.send(response).await;
    }

    Ok(())
}

/// HELPERS

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

//  TODO: factor our with microkernel
fn get_current_unix_time() -> anyhow::Result<u64> {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(t) => Ok(t.as_secs()),
        Err(e) => Err(e.into()),
    }
}

//  WRITERS

pub async fn write_immutable(file_hash: [u8; 32], bytes: &[u8], lfs_directory_path: &PathBuf) -> io::Result<()> {
    let file_name = hex::encode(file_hash);
    let mut file_path = lfs_directory_path.clone();
    file_path.push(file_name);

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&file_path)
        .await?;

    file.write_all(&bytes).await?;

    Ok(())
}

pub async fn write_appendable(
    uuid: u128,
    bytes: &[u8],
    lfs_directory_path: &PathBuf,
) -> io::Result<()> {
    let file_name = uuid.to_string();
    let mut file_path = lfs_directory_path.clone();
    file_path.push(file_name);

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .append(true) // Set append flag
        .open(&file_path)
        .await?;

    file.write_all(bytes).await?;

    Ok(())
}

//  READERS


async fn read_file(start: Option<u64>, length: Option<u64>, file_path: &PathBuf) -> io::Result<Vec<u8>> {
    //  tokio read only by default.
    let mut file = fs::File::open(file_path).await?;
    let mut data = Vec::new();

    if let Some(start_pos) = start {
        file.seek(SeekFrom::Start(start_pos)).await?;
    }

    if let Some(len) = length {
        data.resize(len as usize, 0);
        file.read_exact(&mut data).await?;
    } else {
        file.read_to_end(&mut data).await?;
    }

    Ok(data)
}

pub async fn read_appendable(uuid: u128, lfs_directory_path: &PathBuf, start: Option<u64>, length: Option<u64>) -> Result<Vec<u8>, FileSystemError> {
    let file_name = uuid.to_string();
    let mut file_path = lfs_directory_path.clone();
    file_path.push(file_name.clone());

    read_file(start, length, &file_path).await
        .map_err(|_| FileSystemError::LFSError {
            error: format!("failed reading immutable file {}", file_name)
        })
}

pub async fn read_immutable(file_hash: &[u8; 32], lfs_directory_path: &PathBuf, start: Option<u64>, length: Option<u64>) -> Result<Vec<u8>, FileSystemError> {
    let file_name = hex::encode(file_hash);
    let mut file_path = lfs_directory_path.clone();
    file_path.push(file_name.clone());

    read_file(start, length, &file_path).await
        .map_err(|_| FileSystemError::LFSError {
            error: format!("failed reading immutable file {}", file_name)
        })
}

fn make_error_message(
    our_name: String,
    id: u64,
    source_process: String,
    error: FileSystemError,
) -> WrappedMessage {
    WrappedMessage {
        id,
        target: ProcessNode {
            node: our_name.clone(),
            process: source_process,
        },
        rsvp: None,
        message: Err(UqbarError {
            source: ProcessNode {
                node: our_name,
                process: "lfs".into(),
            },
            timestamp: get_current_unix_time().unwrap(), //  TODO: handle error?
            content: UqbarErrorContent {
                kind: error.kind().into(),
                // message: format!("{}", error),
                message: serde_json::to_value(error).unwrap(), //  TODO: handle error?
                context: serde_json::to_value("").unwrap(),
            },
        }),
    }
}

pub async fn pm_bootstrap(
    home_directory_path: String,
) -> Result<Option<(Vec<u8>, [u8; 32])>, FileSystemError> {
    // fs bootstrapping, create home_directory and manifest file if none.
    // note similarity 
    
    if let Err(e) = create_dir_if_dne(&home_directory_path).await {
        panic!("{}", e);
    }
    let lfs_directory_path_str = format!("{}/lfs", &home_directory_path);

    if let Err(e) = create_dir_if_dne(&lfs_directory_path_str).await {
        panic!("{}", e);
    }
    let lfs_directory_path: std::path::PathBuf =
        fs::canonicalize(lfs_directory_path_str).await.unwrap();

    //  open and load manifest+log

    let manifest_path = lfs_directory_path.join("manifest.bin");

    let mut manifest_file = fs::OpenOptions::new()
        .append(true)
        .read(true)
        .create(true)
        .open(&manifest_path)
        .await
        .expect("fs: failed to open manifest file");

    //  in memory details about files.
    let manifest = Manifest::load(manifest_file, &lfs_directory_path)
        .await
        .map_err(|_| FileSystemError::LFSError {
            error: "loading manifest log failed".into(),
        })?;


    // check for the pm_uuid entry in the manifest
    if let Some(pm_file) = manifest.get_by_uuid(&PM_UUID).await {
        // if found, read and return
        if let Ok(read) = read_immutable(&pm_file.hash, &lfs_directory_path, None, None).await {
            return Ok(Some((read, pm_file.hash)));
        }
    }

    // If pm_uuid entry not found
    Ok(None)
}
