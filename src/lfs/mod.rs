/// log structured filesystem
/// immutable/append []
use anyhow::Result;
use blake3::Hasher;
use hex;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use tokio::fs;
use tokio::io::{self, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::time::{interval, Duration};
use uuid;

use crate::lfs::manifest::{BackupEntry, FileType, InMemoryFile, Manifest};
use crate::lfs::wal::{ToDelete, WAL};
use crate::types::*;
mod manifest;
mod wal;

pub async fn bootstrap(
    our_name: String,
    home_directory_path: String,
) -> Result<
    (
        HashMap<ProcessId, (u128, OnPanic, HashSet<Capability>)>,
        Manifest,
        WAL,
        PathBuf,
    ),
    FileSystemError,
> {
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
    let manifest = Manifest::load(manifest_file, &lfs_directory_path)
        .await
        .expect("manifest load failed!");

    let wal = WAL::load(wal_file, &lfs_directory_path)
        .await
        .expect("wal load failed!");

    // mimic the FsAction::GetState case and get current state of ProcessId::name("kernel")
    // serialize it to a ProcessHandles from process id to JoinHandle

    let kernel_process_id: ProcessId = ProcessId::Name("kernel".into());
    let mut state_map: HashMap<ProcessId, (u128, OnPanic, HashSet<Capability>)> = HashMap::new();

    // get current processes' wasm_bytes handles. GetState(kernel)
    if let Some(file) = manifest.get_by_process(&kernel_process_id).await {
        match read_immutable(&file.hash, &lfs_directory_path, None, None).await {
            Ok(bytes) => {
                state_map =
                    bincode::deserialize(&bytes).expect("kernel state deserialization error!");
            }
            Err(_) => {
                // first time!
            }
        }
    }

    // NOTE OnPanic
    // wasm bytes initial source of truth is the compiled .wasm file on-disk, but onPanic needs to come from somewhere to.
    // for apps in /modules, special cases can be defined here.

    // we also add special-case capabilities spawned "out of thin air" here, for distro processes.
    // at the moment, all bootstrapped processes are given the capability to message all others.
    // this can be easily changed in the future.
    // they are also given access to all runtime modules by name
    let names_and_bytes = get_processes_from_directories().await;

    let mut special_capabilities: HashSet<Capability> = HashSet::new();
    for (process_name, _) in &names_and_bytes {
        special_capabilities.insert(Capability {
            issuer: Address {
                node: our_name.clone(),
                process: ProcessId::Name(process_name.into()),
            },
            label: "messaging".into(),
            params: Some(serde_json::to_string(&ProcessId::Name(process_name.into())).unwrap()),
        });
    }
    for runtime_module in vec![
        "filesystem",
        "http_server",
        "http_client",
        "encryptor",
        "lfs",
        "net",
    ] {
        special_capabilities.insert(Capability {
            issuer: Address {
                node: our_name.clone(),
                process: ProcessId::Name(runtime_module.into()),
            },
            label: "messaging".into(),
            params: Some(serde_json::to_string(&ProcessId::Name(runtime_module.into())).unwrap()),
        });
    }
    // give all distro processes the ability to send messages across the network
    special_capabilities.insert(Capability {
        issuer: Address {
            node: our_name,
            process: ProcessId::Name("kernel".into()),
        },
        label: "network".into(),
        params: None,
    });

    let mut special_on_panics: HashMap<String, OnPanic> = HashMap::new();
    special_on_panics.insert("terminal".into(), OnPanic::Restart);

    // for a module in /modules, put it's bytes into filesystem, add to state_map
    for (process_name, wasm_bytes) in names_and_bytes {
        let hash: [u8; 32] = blake3::hash(&wasm_bytes).into();

        let on_panic = special_on_panics
            .get(&process_name)
            .unwrap_or(&OnPanic::None);

        if let Some((_file, handle)) = manifest.get_by_hash(&hash).await {
            state_map.insert(
                ProcessId::Name(process_name),
                (handle, on_panic.clone(), special_capabilities.clone()),
            );
        } else {
            //  FsAction::Write
            let file_uuid = uuid::Uuid::new_v4().as_u128();
            let file_length = wasm_bytes.len() as u64;

            let _ = write_immutable(hash, &wasm_bytes, &lfs_directory_path).await;
            let backup = BackupEntry {
                file_uuid,
                file_hash: hash,
                file_type: FileType::Immutable,
                process: None,
                file_length,
                backup: Vec::new(),
                local: true,
            };
            let _ = manifest.add_local(&backup).await;
            state_map.insert(
                ProcessId::Name(process_name),
                (file_uuid, on_panic.clone(), special_capabilities.clone()),
            );
        }
    }

    // save kernel process state. FsAction::SetState(kernel)
    let serialized_state_map =
        bincode::serialize(&state_map).expect("state map serialization error!");
    let state_map_hash: [u8; 32] = blake3::hash(&serialized_state_map).into();

    if manifest.get_by_hash(&state_map_hash).await.is_none() {
        let file_uuid = uuid::Uuid::new_v4().as_u128();
        let file_length = serialized_state_map.len() as u64;

        let _ = write_immutable(state_map_hash, &serialized_state_map, &lfs_directory_path).await;
        let backup = BackupEntry {
            file_uuid,
            file_hash: state_map_hash,
            file_type: FileType::Immutable,
            process: Some(ProcessId::Name("kernel".into())),
            file_length,
            backup: Vec::new(),
            local: true,
        };
        let _ = manifest.add_local(&backup).await;
    }

    Ok((state_map, manifest, wal, lfs_directory_path))
}

async fn get_processes_from_directories() -> Vec<(String, Vec<u8>)> {
    let mut processes = Vec::new();

    // Get the path to the /modules directory
    let modules_path = std::path::Path::new("modules");

    // Read the /modules directory
    if let Ok(mut entries) = fs::read_dir(modules_path).await {
        // Loop through the entries in the directory
        while let Ok(Some(entry)) = entries.next_entry().await {
            // If the entry is a directory, add its name to the list of processes
            if let Ok(metadata) = entry.metadata().await {
                if metadata.is_dir() {
                    if let Some(name) = entry.file_name().to_str() {
                        // Get the path to the wasm file for the process
                        let wasm_path = format!(
                            "modules/{}/target/wasm32-unknown-unknown/release/{}.wasm",
                            name, name
                        );
                        // Read the wasm file
                        if let Ok(wasm_bytes) = fs::read(wasm_path).await {
                            // Add the process name and wasm bytes to the list of processes
                            processes.push((name.to_string(), wasm_bytes));
                        }
                    }
                }
            }
        }
    }

    processes
}

pub async fn fs_sender(
    our_name: String,
    lfs_directory_path: PathBuf,
    manifest: Manifest,
    wal: WAL,
    send_to_loop: MessageSender,
    send_to_terminal: PrintSender,
    mut recv_in_fs: MessageReceiver,
) -> Result<()> {
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
        metadata, // for kernel
        ..
    }) = message
    else {
        return Err(FileSystemError::BadJson {
            json: "".into(),
            error: "not a Request with payload".into(),
        });
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
                return Err(FileSystemError::BadBytes {
                    action: "Write".into(),
                });
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
                let write_result =
                    write_immutable(file_hash, &payload.bytes, &lfs_directory_path).await;
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
        }
        FsAction::Read(file_uuid) => {
            // obtain read locks.
            match manifest.read(file_uuid, None, None).await {
                Err(e) => return Err(e),
                Ok(data) => (
                    Some(serde_json::to_string(&FsResponse::Read(file_uuid)).unwrap()),
                    Some(data),
                ),
            }
        }
        FsAction::ReadChunk(req) => {
            match manifest
                .read(req.file_uuid, Some(req.start), Some(req.length))
                .await
            {
                Err(e) => return Err(e),
                Ok(data) => (
                    Some(serde_json::to_string(&FsResponse::ReadChunk(req.file_uuid)).unwrap()),
                    Some(data),
                ),
            }
        }
        FsAction::Replace(old_file_uuid) => {
            let Some(ref payload) = payload else {
                return Err(FileSystemError::BadBytes {
                    action: "Write".into(),
                });
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
                let write_result =
                    write_immutable(file_hash, &payload.bytes, &lfs_directory_path).await;
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
        }
        FsAction::Delete(del) => {
            // let (file, uuid) = manifest.get_by_hash(&del).await.ok_or(FileSystemError::LFSError {
            //     error: format!("no file found for hash: {:?}", del),
            // })?;

            (
                Some(serde_json::to_string(&FsResponse::Delete(del)).unwrap()),
                None,
            )
        }
        FsAction::Append(maybe_file_uuid) => {
            let Some(ref payload) = payload else {
                return Err(FileSystemError::BadBytes {
                    action: "Write".into(),
                });
            };

            let (file, uuid) = if let Some(existing_file_uuid) = maybe_file_uuid {
                match manifest.get_by_uuid(&existing_file_uuid).await {
                    Some(file) => (file, existing_file_uuid),
                    None => {
                        return Err(FileSystemError::LFSError {
                            error: format!("no file found for id: {:?}", existing_file_uuid),
                        })
                    }
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

            let _ = manifest
                .insert(
                    uuid,
                    InMemoryFile {
                        hash: new_hash,
                        file_type: FileType::Appendable,
                        hasher: temp_hasher,
                        file_length: file.file_length + chunk_length,
                    },
                )
                .await?;

            (
                Some(serde_json::to_string(&FsResponse::Append(uuid)).unwrap()),
                None,
            )
        }
        FsAction::Length(file_uuid) => match manifest.get_by_uuid(&file_uuid).await {
            None => {
                return Err(FileSystemError::LFSError {
                    error: format!("no file found for id: {:?}", file_uuid),
                })
            }
            Some(file) => (
                Some(serde_json::to_string(&FsResponse::Length(file.file_length)).unwrap()),
                None,
            ),
        },
        //  process state handlers
        FsAction::SetState => {
            let Some(ref payload) = payload else {
                return Err(FileSystemError::BadBytes {
                    action: "Write".into(),
                });
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

            //  write underlying
            let write_result =
                write_immutable(file_hash, &payload.bytes, &lfs_directory_path).await;
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
        }
        FsAction::GetState => {
            if let Some(file) = manifest.get_by_process(&source.process).await {
                // match on filetype possible, in this case assumed immutable
                match read_immutable(&file.hash, &lfs_directory_path, None, None).await {
                    Ok(bytes) => (
                        Some(serde_json::to_string(&FsResponse::GetState).unwrap()),
                        Some(bytes),
                    ),
                    Err(_) => (
                        Some(serde_json::to_string(&FsResponse::GetState).unwrap()),
                        None,
                    ),
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
            message: Message::Response((
                Ok(Response {
                    ipc,
                    metadata, // for kernel
                }),
                None,
            )),
            payload: Some(Payload {
                mime: None,
                bytes: bytes.unwrap_or_default(),
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

//  WRITERS

pub async fn write_immutable(
    file_hash: [u8; 32],
    bytes: &[u8],
    lfs_directory_path: &PathBuf,
) -> io::Result<()> {
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

pub async fn read_file(
    start: Option<u64>,
    length: Option<u64>,
    file_path: &PathBuf,
) -> io::Result<Vec<u8>> {
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

pub async fn read_mutable(
    uuid: u128,
    lfs_directory_path: &PathBuf,
    start: Option<u64>,
    length: Option<u64>,
) -> Result<Vec<u8>, FileSystemError> {
    let file_name = uuid.to_string();
    let mut file_path = lfs_directory_path.clone();
    file_path.push(file_name.clone());

    read_file(start, length, &file_path)
        .await
        .map_err(|_| FileSystemError::LFSError {
            error: format!("failed reading immutable file {}", file_name),
        })
}

pub async fn read_immutable(
    file_hash: &[u8; 32],
    lfs_directory_path: &PathBuf,
    start: Option<u64>,
    length: Option<u64>,
) -> Result<Vec<u8>, FileSystemError> {
    let file_name = hex::encode(file_hash);
    let mut file_path = lfs_directory_path.clone();
    file_path.push(file_name.clone());

    read_file(start, length, &file_path)
        .await
        .map_err(|_| FileSystemError::LFSError {
            error: format!("failed reading immutable file {}", file_name),
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
        message: Message::Response((
            Err(UqbarError {
                kind: error.kind().into(),
                message: Some(serde_json::to_string(&error).unwrap()),
            }),
            None,
        )),
        payload: None,
    }
}
