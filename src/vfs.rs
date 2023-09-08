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

type ResponseRouter = HashMap<u64, MessageSender>;
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
enum Key {
    Dir { id: u64 },
    File { id: FileHash },
    // ...
}
type KeyToEntry = HashMap<Key, Entry>;
type PathToKey = HashMap<String, Key>;
struct Vfs {
    key_to_entry: KeyToEntry,
    path_to_key: PathToKey,
}
type ProcessToVfs = HashMap<ProcessId, Arc<Mutex<Vfs>>>;

#[derive(Clone, Debug)]
struct Entry {
    name: String,
    full_path: String,  //  full_path, ending with `/` for dir
    entry_type: EntryType,
    // ...  //  general metadata?
}
#[derive(Clone, Debug)]
enum EntryType {
    Dir { children: HashSet<Key> },
    File { parent: Key },  //  hash could be generalized to `location` if we want to be able to point at, e.g., remote files
    // File { parent: String, hash: FileHash },  //  hash could be generalized to `location` if we want to be able to point at, e.g., remote files
    // ...  //  symlinks?
}

impl Vfs {
    fn new() -> Self {
        let key_to_entry: KeyToEntry = HashMap::new();
        let path_to_key: PathToKey = HashMap::new();
        let root_path = "/".into();
        let root_key = Key::Dir { id: 0 };
        key_to_entry.insert(
            root_key.clone(),
            Entry {
                name: root_path.clone(),
                full_path: root_path.clone(),
                entry_type: EntryType::Dir { children: HashSet::new() },
            },
        );
        path_to_key.insert(
            root_path.clone(),
            root_key,
        );
        Vfs {
            key_to_entry,
            path_to_key,
        }
    }
}

fn make_dir_name(full_path: &str) -> String {
    let split_path = full_path.split("/");
    let _ = split_path.next_back();
    let name = format!("{}/", split_path.next_back().unwrap());
    // let path: Vec<&str> = split_path.collect();
    // let path = path.join("/");
    // let path =
    //     if path == "" {
    //         ""  //  root case
    //     } else {
    //         format!("{}/", path)
    //     };
    name
}

fn make_file_name(full_path: &str) -> String {
    let split_path = full_path.split("/");
    let name = split_path.next_back().unwrap();
    // let path: Vec<&str> = split_path.collect();
    // let path = format!("{}/", path.join("/"));
    name
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
    _source_process: &str,
    uri_string: &str,
) -> Result<String, FileSystemError> {
    let uri = match uri_string.parse::<Uri>() {
        Ok(uri) => uri,
        Err(_) => {
            return Err(FileSystemError::BadUri {
                uri: uri_string.into(),
                bad_part_name: "entire".into(),
                bad_part: Some(uri_string.into()),
            })
        }
    };

    if Some("fs") != uri.scheme_str() {
        return Err(FileSystemError::BadUri {
            uri: uri_string.into(),
            bad_part_name: "scheme".into(),
            bad_part: match uri.scheme_str() {
                Some(s) => Some(s.into()),
                None => None,
            },
        });
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

    join_paths(home_directory_path.into(), relative_file_path)
}

fn join_paths(base_path: String, relative_path: String) -> Result<String, FileSystemError> {
    match std::path::Path::new(&base_path)
        .join(&relative_path)
        .to_str()
        .ok_or(FileSystemError::BadPathJoin {
            base_path,
            addend: relative_path,
        }) {
        Ok(s) => Ok(s.into()),
        Err(e) => Err(e),
    }
}

fn make_error_message(
    our_name: String,
    id: u64,
    source_process: String,
    error: FileSystemError,
) -> KernelMessage {
    KernelMessage {
        id,
        source: Address {
            node: our_name.clone(),
            process: ProcessId::Name(source_process),
        },
        target: Address {
            node: our_name,
            process: ProcessId::Name("filesystem".into()),
        },
        rsvp: None,
        message: Message::Response((
            Err(UqbarError {
                kind: error.kind().into(),
                message: Some(serde_json::to_string(&error).unwrap()), //  TODO: handle error?
            }),
            None,
        )),
        payload: None,
    }
}

pub async fn vfs(
    our_node: String,
    send_to_loop: MessageSender,
    send_to_terminal: PrintSender,
    mut recv_from_loop: MessageReceiver,
) {
    let mut process_to_vfs: ProcessToVfs = HashMap::new();
    let mut response_router: ResponseRouter = HashMap::new();
    let (send_vfs_task_done, recv_vfs_task_done): (
        tokio::sync::mpsc::Sender<u64>,
        tokio::sync::mpsc::Receiver<u64>,
    ) = mpsc::channel(5);

    loop {
        tokio::select! {
            id_done = recv_vfs_task_done.recv() => {
                response_router.remove(id_done);
            },
            km = recv_from_loop.recv() => {
                if let Some(response_sender) = response_router.get(&km.id) {
                    response_sender.send(km).await.unwrap();
                    continue;
                }

                // let ProcessId::Name(source_process) = &km.source.process else {
                //     panic!("filesystem: require source identifier contain process name")
                //     // return Err(FileSystemError::FsError {
                //     //     what: "to_absolute_path".into(),
                //     //     path: "home_directory_path".into(),
                //     //     error: "need source process name".into(),
                //     // })
                // };
                if our_node != km.source.node {
                    println!(
                        "vfs: request must come from our_node={}, got: {}",
                        our_node,
                    );
                    continue;
                }
                let vfs = Arc::clone(match process_to_vfs.get(&km.source.process) {
                    Some(vfs) => vfs,
                    None => {
                        //  create open_files entry
                        process_to_vfs.insert(
                            km.source.process.into(),
                            Arc::new(Mutex::new(Vfs::new())),
                        );
                        process_to_vfs.get(&km.source.process).unwrap()
                    }
                });
                let our_node = our_node.clone();
                let source_process = km.source.process.into();
                let id = km.id;

                let (response_sender, response_receiver): (
                    MessageSender,
                    MessageReceiver,
                ) = mpsc::channel(2);
                response_router.insert(id.clone(), response_sender);
                let send_to_loop = send_to_loop.clone();
                let send_to_terminal = send_to_terminal.clone();
                let send_vfs_task_done = send_vfs_task_done.clone();
                match &km.message {
                    Message::Response(_) => {},
                    Message::Request(_) => {
                        tokio::spawn(async move {
                            match handle_request(
                                our_node.clone(),
                                km,
                                vfs,
                                send_to_loop.clone(),
                                send_to_terminal,
                                response_receiver,
                            ).await {
                                Err(e) => {
                                    send_to_loop
                                        .send(make_error_message(
                                            our_node.into(),
                                            id,
                                            source_process,
                                            e,
                                        ))
                                        .await
                                        .unwrap();
                                },
                                Ok(_) => {},
                            }
                            send_vfs_task_done.send(id).await.unwrap();
                        });
                    },
                }
            },
        }
    }
    while let Some(km) = recv_in_fs.recv().await {
    }
}

//  TODO: error handling: send error messages to caller
async fn handle_request(
    our_name: String,
    home_directory_path: String,
    kernel_message: KernelMessage,
    vfs: Arc<Mutex<Vfs>>,
    send_to_loop: MessageSender,
    send_to_terminal: PrintSender,
    recv_response: MessageReceiver,
) -> Result<(), FileSystemError> {
    let KernelMessage {
        ref id,
        source,
        target: _,
        rsvp,
        message,
        payload,
    } = kernel_message;
    let Message::Request(Request {
        expects_response,
        ipc: Some(ipc),
        metadata, // we return this to Requester for kernel reasons
        ..
    }) = message else {
        return Err(FileSystemError::BadJson {
            json: "".into(),
            error: "not a Request with payload".into(),
        })
    };

    let request: VfsRequest = match serde_json::from_str(&ipc) {
        Ok(r) => r,
        Err(e) => {
            return Err(FileSystemError::BadJson {
                json: ipc.into(),
                error: format!("parse failed: {:?}", e),
            })
        }
    };

    // let source_process = &source.process;
    let ProcessId::Name(source_process) = &source.process else {
        // panic!("filesystem: require source identifier contain process name")
        return Err(FileSystemError::FsError {
            what: "to_absolute_path".into(),
            path: "home_directory_path".into(),
            error: "need source process name".into(),
        })
    };
    // let file_path = get_file_path(&request.uri_string).await;
    let file_path =
        to_absolute_path(&home_directory_path, source_process, &request.uri_string).await?;
    if HAS_FULL_HOME_ACCESS.contains(source_process) {
        if !std::path::Path::new(&file_path).starts_with(&home_directory_path) {
            return Err(FileSystemError::IllegalAccess {
                process_name: source_process.into(),
                attempted_dir: file_path,
                sandbox_dir: home_directory_path,
            });
        }
    } else {
        let sandbox_dir_path = join_paths(home_directory_path, source_process.into())?;
        if !std::path::Path::new(&file_path).starts_with(&sandbox_dir_path) {
            return Err(FileSystemError::IllegalAccess {
                process_name: source_process.into(),
                attempted_dir: file_path,
                sandbox_dir: sandbox_dir_path,
            });
        }
    }

    let (ipc, bytes) = match request {
        VfsRequest::Add { full_path, entry_type } => {
            match entry_type {
                AddEntryType::Dir => {
                    let full_path =
                        if let Some(last_char) = file_path.chars().last() {
                            if last_char == '/' {
                                full_path
                            } else {
                                format!("{}/", full_path)
                            };
                        } else {
                            panic!("empty path");
                        };
                    let vfs = vfs.lock().await;
                    if vfs.path_to_key.contains_key(&full_path) {
                        send_to_terminal
                            .send(Printout {
                                verbosity: 0,
                                content: &format!("vfs: not overwriting dir {}", full_path),
                            })
                            .await
                            .unwrap();
                        panic!("");  //  TODO: error?
                    };
                    let name = make_dir_name(&full_path);
                    let key = Key::Dir { id: rand::random() };
                    vfs.key_to_entry.insert(
                        key.clone(),
                        Entry {
                            name,
                            full_path,
                            entry_type: EntryType::Dir { children: HashSet::new() },
                        },
                    );
                    vfs.path_to_key.insert(
                        full_path,
                        key,
                    );
                },
                AddEntryType::File { hash } => {
                    if let Some(last_char) = file_path.chars().last() {
                        if last_char == '/' {
                            send_to_terminal
                                .send(Printout {
                                    verbosity: 0,
                                    content: &format!(
                                        "vfs: file path cannot end with `/`: {}",
                                        full_path,
                                    ),
                                })
                                .await
                                .unwrap();
                            panic!("");
                        }
                    } else {
                        panic!("empty path");
                    };
                    let vfs = vfs.lock().await;
                    if vfs.path_to_key.contains_key(&full_path) {
                        send_to_terminal
                            .send(Printout {
                                verbosity: 1,
                                content: &format!("vfs: overwriting file {}", full_path),
                            })
                            .await
                            .unwrap();
                        let Some(old_key) = vfs.path_to_key.remove(&file_path) else {
                            panic!("no old key");
                        };
                        vfs.key_to_entry.remove(&old_key);
                    };
                    let name = make_file_name(&full_path);
                    let key = Key::File { id: hash.clone() };
                    vfs.key_to_entry.insert(
                        key.clone(),
                        Entry {
                            name,
                            full_path,
                            entry_type: EntryType::File {
                                parent,
                            },
                        },
                    );
                    vfs.path_to_key.insert(
                        full_path,
                        key,
                    );
                },
            }
            (
                Some(serde_json::to_string(VfsResponse::Add).unwrap()),
                None,
            )
        },
        VfsRequest::Rename { full_path, new_full_path } => {
            let vfs = vfs.lock().await;
            let Some(key) = vfs.path_to_key.remove(&full_path) else {
                send_to_terminal
                    .send(Printout {
                        verbosity: 0,
                        content: &format!("vfs: can't rename: nonexistent file {}", full_path),
                    })
                    .await
                    .unwrap();
                panic!("");
            };
            let Some(entry) = vfs.key_to_entry.remove(&key) else {
                send_to_terminal
                    .send(Printout {
                        verbosity: 0,
                        content: &format!("vfs: can't rename: nonexistent file {}", full_path),
                    })
                    .await
                    .unwrap();
                panic!("");
            };
            match entry.entry_type {
                EntryType::Dir { children } => {
                    if vfs.path_to_key.contains_key(&new_full_path) {
                        send_to_terminal
                            .send(Printout {
                                verbosity: 0,
                                content: &format!("vfs: not overwriting dir {}", new_full_path),
                            })
                            .await
                            .unwrap();
                        vfs.path_to_key.insert(full_path, entry);
                        panic!("");  //  TODO: error?
                    };
                    let name = make_dir_name(&new_full_path);
                    entry.name = name;
                    entry.full_path = new_full_path.clone();
                    vfs.path_to_key.insert(new_full_path.clone(), key.clone());
                    vfs.key_to_entry.insert(key, entry);
                    //  TODO: recursively apply path update to all children
                    //  update_child_paths(full_path, new_full_path, children);
                },
                EntryType::File { parent: _ } => {
                    if vfs.path_to_key.contains_key(&new_full_path) {
                        send_to_terminal
                            .send(Printout {
                                verbosity: 1,
                                content: &format!("vfs: overwriting file {}", new_full_path),
                            })
                            .await
                            .unwrap();
                    };
                    let name = make_file_name(&new_full_path);
                    entry.name = name;
                    entry.full_path = new_full_path.clone();
                    vfs.path_to_key.insert(new_full_path, key.clone());
                    vfs.key_to_entry.insert(key, entry);
                },
            }
            (
                Some(serde_json::to_string(VfsResponse::Rename).unwrap()),
                None,
            )
        },
        VfsRequest::Delete { full_path } => {
            let vfs = vfs.lock().await;
            let Some(key) = vfs.path_to_key.remove(&full_path) else {
                send_to_terminal
                    .send(Printout {
                        verbosity: 0,
                        content: &format!("vfs: can't delete: nonexistent entry {}", full_path),
                    })
                    .await
                    .unwrap();
                panic!("");
            };
            let Some(entry) = vfs.key_to_entry.remove(&key) else {
                send_to_terminal
                    .send(Printout {
                        verbosity: 0,
                        content: &format!("vfs: can't delete: nonexistent entry {}", full_path),
                    })
                    .await
                    .unwrap();
                panic!("");
            };
            match entry.entry_type {
                EntryType::Dir { children } => {
                    if !children.is_empty() {
                        send_to_terminal
                            .send(Printout {
                                verbosity: 0,
                                content: &format!("vfs: can't delete: non-empty directory {}", full_path),
                            })
                            .await
                            .unwrap();
                        vfs.path_to_key.insert(full_path, key.clone());
                        vfs.key_to_entry.insert(key.clone(), entry);
                    }
                },
                EntryType::File { parent } => {
                    match vfs.key_to_entry.get_mut(&parent) {
                        None => {
                            send_to_terminal
                                .send(Printout {
                                    verbosity: 0,
                                    content: &format!("vfs: delete: unexpected file with no parent dir: {}", full_path),
                                })
                                .await
                                .unwrap();
                            panic!("");
                        },
                        Some(parent) => {
                            let EntryType::Dir { ref children } = parent.entry_type else {
                                panic!("");
                            };
                            //  TODO: does this work?
                            children.remove(key);
                        }
                    }
                },
            }
            (
                Some(serde_json::to_string(VfsResponse::Delete).unwrap()),
                None,
            )
        },
        VfsRequest::GetPath { hash } => {
            let vfs = vfs.lock().await;
            let key = Key::File { id: hash };
            match vfs.key_to_entry.remove(&key) {
                None => (None, None),
                Some(entry) => {
                    let full_path = entry.full_path.clone();
                    vfs.key_to_entry.insert(key, entry);
                    (
                        Some(serde_json::to_string(VfsResponse::GetPath {
                            full_path,
                        }).unwrap()),
                        None,
                    )
                },
            }
        },
        VfsRequest::GetEntry { full_path } => {
            //  dir => return children
            //  file => get file bytes from lfs; send here; recv_response; return bytes
        },
    };

    //  TODO: properly handle rsvp
    if expects_response {
        let response = KernelMessage {
            id: *id,
            source: Address {
                node: our_name.clone(),
                process: ProcessId::Name("filesystem".into()),
            },
            target: Address {
                node: our_name.clone(),
                process: source.process.clone(),
            },
            rsvp,
            message: Message::Response((
                Ok(Response {
                    ipc,
                    metadata,
                }),
                None,
            )),
            payload: match bytes {
                Some(bytes) => Some(Payload {
                    mime: Some("application/octet-stream".into()),
                    bytes,
                }),
                None => None,
            },
        };

        let _ = send_to_loop.send(response).await;
    }

    Ok(())
}
