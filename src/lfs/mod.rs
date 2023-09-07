/// log structured filesystem
/// immutable/append []
use blake3::Hasher;
use hex;
use std::path::PathBuf;
use tokio::fs;
use tokio::io::{self, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::time::{Duration, interval};
use uuid;

use crate::types::*;
use crate::lfs::manifest::{Manifest, BackupEntry, FileType, InMemoryFile};
use crate::lfs::wal::{WAL, ToDelete};
mod manifest;
mod wal;


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

    let manifest_file = fs::OpenOptions::new()
        .append(true)
        .read(true)
        .create(true)
        .open(&manifest_path)
        .await
        .expect("fs: failed to open manifest file");

    let wal_path = lfs_directory_path.join("wal.bin");

    let wal_file = fs::OpenOptions::new()
        .append(true)
        .read(true)
        .create(true)
        .open(&wal_path)
        .await
        .expect("fs: failed to open WAL file");

    //  in memory details about files.
    let manifest = Manifest::load(manifest_file, &lfs_directory_path).await.expect("manifest load failed!");

    //  println!("whole manifest {:?}", manifest);
    //  let wal_file = Arc::new(RwLock::new(wal_file));

    let wal = WAL::load(wal_file, &lfs_directory_path).await.expect("wal load failed!");

    //  interval for deleting/(flushing)
    let mut interval = interval(Duration::from_secs(60));

    //  into main loop
    loop {
        tokio::select! {
            Some(kernel_message) = recv_in_fs.recv() => {
                if our_name != kernel_message.source.node {
                    println!(
                        "lfs: request must come from our_name={}, got: {}",
                        our_name, &kernel_message,
                    );
                    continue;
                }

            //  internal structures have Arc::clone setup.
            let manifest_clone = manifest.clone();
            let wal_clone = wal.clone();

            // let wal_file_clone = wal_file.clone();
            let lfs_directory_path_clone = lfs_directory_path.clone();
            let our_name = our_name.clone();
            let source = kernel_message.source.clone();
            let id = kernel_message.id;
            let send_to_loop = send_to_loop.clone();
            let send_to_terminal = send_to_terminal.clone();

            tokio::spawn(async move {
                if let Err(e) = handle_request(
                    our_name.clone(),
                    kernel_message,
                    lfs_directory_path_clone,
                    manifest_clone,
                    wal_clone,
                    send_to_loop.clone(),
                    send_to_terminal,
                )
                .await
                {
                    send_to_loop
                        .send(make_error_message(our_name.into(), id, source, e))
                        .await
                        .unwrap();
                }
            });
            }
            _ = interval.tick() => {
                //  println!("in flush");
                //  note be careful about deadlocks in this case.
                //  let wal_clone = wal.clone();
                let manifest_clone = manifest.clone();

                tokio::spawn(async move {
                    let _ = manifest_clone.cleanup().await;
                });

            }
        }
    }
}

async fn handle_request(
    our_name: String,
    kernel_message: KernelMessage,
    lfs_directory_path: PathBuf,
    manifest: Manifest,
    wal: WAL,
    send_to_loop: MessageSender,
    _send_to_terminal: PrintSender,
) -> Result<(), FileSystemError> {
    let KernelMessage {
        id,
        source,
        rsvp,
        message,
        payload,
        ..
    } = kernel_message;
    let Message::Request(Request {
        expects_response,
        ipc: Some(json_string),
        ..
    }) = message else {
        return Err(FileSystemError::BadJson {
            json: "".into(),
            error: "not a Request with payload".into(),
        })
    };

    let action: FsAction = match serde_json::from_str(&json_string) {
        Ok(r) => r,
        Err(e) => {
            return Err(FileSystemError::BadJson {
                json: json_string.into(),
                error: format!("parse failed: {:?}", e),
            })
        }
    };

    //  println!("got action! {:?}", action);

    let (ipc, bytes) = match action {
        FsAction::Write => {
            let Some(ref payload) = payload else {
                return Err(FileSystemError::BadBytes { action: "Write".into() })
            };

            let file_uuid = uuid::Uuid::new_v4().as_u128();
            let file_length = payload.bytes.len() as u64;

            let mut hasher = blake3::Hasher::new();
            hasher.update(&payload.bytes);
            let file_hash: [u8; 32] = hasher.finalize().into();

            //  if file exists, just return.
            if let Some((_, uuid)) = manifest.get_by_hash(&file_hash).await {
                (
                    Some(serde_json::to_string(&FsResponse::Write(uuid)).unwrap()),
                    None,
                )
            } else {
            //  create and write underlying
            let write_result = write_immutable(file_hash, &payload.bytes, &lfs_directory_path).await;
            if let Err(e) = write_result {
                return Err(FileSystemError::LFSError {
                    error: format!("write failed: {}", e),
                });
            } else {
                //  append to manifest
                let backup = BackupEntry {
                    file_uuid,
                    file_hash,
                    file_type: FileType::Immutable,
                    process: None,
                    file_length,
                    backup: Vec::new(),
                    local: true,
                };

                let _ = manifest.add_local(&backup).await?;
            }

            (
                Some(serde_json::to_string(&FsResponse::Write(file_uuid)).unwrap()),
                None,
            )
            }
        },
        FsAction::Read(file_uuid) => {
            // obtain read locks.
            match manifest.read(file_uuid, None, None).await {
                Err(e) => return Err(e),
                Ok(data) => {
                    (
                        Some(serde_json::to_string(&FsResponse::Read(file_uuid)).unwrap()),
                        Some(data),
                    )
                }
            }
        },
        FsAction::ReadChunk(req) => {
            match manifest.read(req.file_uuid, Some(req.start), Some(req.length)).await {
                Err(e) => return Err(e),
                Ok(data) => {
                    (
                        Some(serde_json::to_string(&FsResponse::ReadChunk(req.file_uuid)).unwrap()),
                        Some(data),
                    )
                }
            }
        },
        FsAction::Replace(old_file_uuid) => {
            let Some(ref payload) = payload else {
                return Err(FileSystemError::BadBytes { action: "Write".into() })
            };

            let file_uuid = uuid::Uuid::new_v4().as_u128();
            let file_length = payload.bytes.len() as u64;

            let mut hasher = blake3::Hasher::new();
            hasher.update(&payload.bytes);
            let file_hash: [u8; 32] = hasher.finalize().into();

            // add delete entry for previous hash, if it exists
            if let Some(_) = manifest.get_by_uuid(&old_file_uuid).await {
                let _ = wal.add_delete(ToDelete::Immutable(file_hash)).await;
            }


            //  if file exists, just return.
            if let Some((file, uuid)) = manifest.get_by_hash(&file_hash).await {
                (
                    Some(serde_json::to_string(&FsResponse::Write(uuid)).unwrap()),
                    None,
                )
            } else {

            //  create and write underlying
            let write_result = write_immutable(file_hash, &payload.bytes, &lfs_directory_path).await;
            if let Err(e) = write_result {
                return Err(FileSystemError::LFSError {
                    error: format!("write failed: {}", e),
                });
            } else {
                //  append to manifest
                let backup = BackupEntry {
                    file_uuid,
                    file_hash,
                    file_type: FileType::Immutable,
                    process: None,
                    file_length,
                    backup: Vec::new(),
                    local: true,
                };


                let _ = manifest.add_local(&backup).await?;
            }

            (
                Some(serde_json::to_string(&FsResponse::Write(file_uuid)).unwrap()),
                None,
            )
            }
        },
        FsAction::Delete(del) => {
            // let (file, uuid) = manifest.get_by_hash(&del).await.ok_or(FileSystemError::LFSError {
            //     error: format!("no file found for hash: {:?}", del),
            // })?;

            (
                Some(serde_json::to_string(&FsResponse::Delete(del)).unwrap()),
                None,
            )
        },
        FsAction::Append(maybe_file_uuid) => {
            let Some(ref payload) = payload else {
                return Err(FileSystemError::BadBytes { action: "Write".into() })
            };

            let (file, uuid) = if let Some(existing_file_uuid) = maybe_file_uuid {
                match manifest.get_by_uuid(&existing_file_uuid).await {
                    Some(file) => {
                        (file, existing_file_uuid)
                    },
                    None => return Err(FileSystemError::LFSError {
                        error: format!("no file found for id: {:?}", existing_file_uuid),
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

            let chunk_length = payload.bytes.len() as u64;
            let mut temp_hasher = file.hasher.clone();
            temp_hasher.update(&payload.bytes);
            let new_hash: [u8; 32] = temp_hasher.finalize().into();

            let write_result = write_appendable(uuid, &payload.bytes, &lfs_directory_path).await;
            if let Err(e) = write_result {
                return Err(FileSystemError::LFSError {
                    error: format!("write failed: {}", e),
                });
            }

            let backup = BackupEntry {
                file_uuid: uuid,
                file_hash: new_hash,
                file_type: FileType::Appendable,
                process: None,
                file_length: file.file_length + chunk_length,
                backup: Vec::new(),
                local: true,
            };
            let _ = manifest.add_local(&backup).await?;

            let _ = manifest.insert(uuid,InMemoryFile {
                    hash: new_hash,
                    file_type: FileType::Appendable,
                    hasher: temp_hasher,
                    file_length: file.file_length + chunk_length,
                },
            ).await?;

            (
                Some(serde_json::to_string(&FsResponse::Append(uuid)).unwrap()),
                None,
            )
        },
        FsAction::Length(file_uuid) => {
            match manifest.get_by_uuid(&file_uuid).await {
                None => return Err(FileSystemError::LFSError {
                    error: format!("no file found for id: {:?}", file_uuid),
                }),
                Some(file) => {
                    (
                        Some(serde_json::to_string(&FsResponse::Length(file.file_length)).unwrap()),
                        None,
                    )
                }
            }
        },
        //  process state handlers
        FsAction::SetState => {
            let Some(ref payload) = payload else {
                return Err(FileSystemError::BadBytes { action: "Write".into() })
            };


            let file_uuid = uuid::Uuid::new_v4().as_u128();

            //  create new immutable file
            let mut hasher = blake3::Hasher::new();
            hasher.update(&payload.bytes);
            let file_hash: [u8; 32] = hasher.finalize().into();

            //  deduplication is delicate, here we do only check for same state as before.
            //   if let Some(file) = manifest.get_by_process(&source.process).await {
            //       if file.hash == file_hash {
            //           (
            //               Some(serde_json::to_string(&FsResponse::SetState).unwrap()),
            //               None,
            //           )
            //       }
            //   } else {
            //  let prev_hash = manifest.get_by_uuid(&).await.map(|file| file.hash);
            //  remove previous hash!

            //  write underlying
            let write_result = write_immutable(file_hash, &payload.bytes, &lfs_directory_path).await;
            if let Err(e) = write_result {
                return Err(FileSystemError::LFSError {
                    error: format!("write failed: {}", e),
                });
            } else {
                //  append to manifest
                let backup = BackupEntry {
                    file_uuid,
                    file_hash,
                    file_type: FileType::Immutable,
                    local: true,
                    process: Some(source.process.clone()),
                    file_length: payload.bytes.len() as u64,
                    backup: Vec::new(),
                };

                let _ = manifest.add_local(&backup).await?;
            }

            (
                Some(serde_json::to_string(&FsResponse::SetState).unwrap()),
                None,
            )

        },
        FsAction::GetState => {
            if let Some(file) = manifest.get_by_process(&source.process).await {
                // match on filetype possible, in this case assumed immutable
                match read_immutable(&file.hash, &lfs_directory_path, None, None).await {
                    Ok(bytes) => {
                        (
                            Some(serde_json::to_string(&FsResponse::GetState).unwrap()),
                            Some(bytes),
                        )
                    },
                    Err(_) => {
                        (
                            Some(serde_json::to_string(&FsResponse::GetState).unwrap()),
                            None,
                        )
                    }
                }
            } else {
                (
                    Some(serde_json::to_string(&FsResponse::GetState).unwrap()),
                    None,
                )
            }
        }
    };

    if expects_response {
        let response = KernelMessage {
            id: id.clone(),
            source: Address {
                node: our_name.clone(),
                process: ProcessId::Name("lfs".into()),
            },
            target: source.clone(),
            rsvp,
            message: Message::Response((Ok(Response {
                ipc,
                metadata: None,
            }), None)),
            payload: Some(
                Payload {
                    mime: None,
                    bytes: bytes.unwrap_or_default(),
                }
            )
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


pub async fn read_file(start: Option<u64>, length: Option<u64>, file_path: &PathBuf) -> io::Result<Vec<u8>> {
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

pub async fn read_mutable(uuid: u128, lfs_directory_path: &PathBuf, start: Option<u64>, length: Option<u64>) -> Result<Vec<u8>, FileSystemError> {
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
    target: Address,
    error: FileSystemError,
) -> KernelMessage {
    KernelMessage {
        id,
        source: Address {
            node: our_name.clone(),
            process: ProcessId::Name("lfs".into()),
        },
        target,
        rsvp: None,
        message: Message::Response((Err(UqbarError {
            kind: error.kind().into(),
            message: Some(serde_json::to_string(&error).unwrap()),
            }), None)),
        payload: None,
    }
}

//  pub async fn pm_bootstrap(
//      home_directory_path: String,
//  ) -> Result<Option<(Vec<u8>, [u8; 32])>, FileSystemError> {
//  don't need for now, but will probably in future with app distribution etc.