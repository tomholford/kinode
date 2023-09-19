use bytes::Bytes;
use http::Uri;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use sha2::Sha256;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::sync::Mutex;

use crate::types::*;

const VFS_PERSIST_STATE_CHANNEL_CAPACITY: usize = 5;
const VFS_TASK_DONE_CHANNEL_CAPACITY: usize = 5;
const VFS_RESPONSE_CHANNEL_CAPACITY: usize = 2;

type ResponseRouter = HashMap<u64, MessageSender>;
#[derive(Clone, Debug, Eq, PartialEq, Hash, Deserialize, Serialize)]
enum Key {
    Dir { id: u64 },
    File { id: u128 },
    // ...
}
type KeyToEntry = HashMap<Key, Entry>;
type PathToKey = HashMap<String, Key>;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Vfs {
    key_to_entry: KeyToEntry,
    path_to_key: PathToKey,
}
type ProcessToVfs = HashMap<ProcessId, Arc<Mutex<Vfs>>>;
type ProcessToVfsSerializable = HashMap<ProcessId, Vfs>;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Entry {
    name: String,
    full_path: String, //  full_path, ending with `/` for dir
    entry_type: EntryType,
    // ...  //  general metadata?
}
#[derive(Clone, Debug, Deserialize, Serialize)]
enum EntryType {
    Dir { parent: Key, children: HashSet<Key> },
    File { parent: Key }, //  hash could be generalized to `location` if we want to be able to point at, e.g., remote files
                          // ...  //  symlinks?
}

impl Vfs {
    fn new() -> Self {
        let mut key_to_entry: KeyToEntry = HashMap::new();
        let mut path_to_key: PathToKey = HashMap::new();
        let root_path: String = "/".into();
        let root_key = Key::Dir { id: 0 };
        key_to_entry.insert(
            root_key.clone(),
            Entry {
                name: root_path.clone(),
                full_path: root_path.clone(),
                entry_type: EntryType::Dir {
                    parent: root_key.clone(),
                    children: HashSet::new(),
                },
            },
        );
        path_to_key.insert(root_path.clone(), root_key.clone());
        Vfs {
            key_to_entry,
            path_to_key,
        }
    }
}

fn make_dir_name(full_path: &str) -> (String, String) {
    if full_path == "/" {
        return ("/".into(), "".into()); //  root case
    }
    let mut split_path: Vec<&str> = full_path.split("/").collect();
    let _ = split_path.pop();
    let name = format!("{}/", split_path.pop().unwrap());
    let path = split_path.join("/");
    let path = if path == "" {
        "/".into()
    } else {
        format!("{}/", path)
    };
    (name, path)
}

fn make_file_name(full_path: &str) -> (String, String) {
    let mut split_path: Vec<&str> = full_path.split("/").collect();
    let name = split_path.pop().unwrap();
    let path = format!("{}/", split_path.join("/"));
    (name.into(), path)
}

fn make_error_message(
    our_name: String,
    id: u64,
    source: Address,
    error: VfsError,
) -> KernelMessage {
    KernelMessage {
        id,
        source: Address {
            node: our_name,
            process: ProcessId::Name("vfs".into()),
        },
        target: source,
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

async fn state_to_bytes(state: &ProcessToVfs) -> Vec<u8> {
    let mut serializable: ProcessToVfsSerializable = HashMap::new();
    for (process_id, vfs) in state.iter() {
        let vfs = vfs.lock().await;
        serializable.insert(process_id.clone(), (*vfs).clone());
    }
    bincode::serialize(&serializable).unwrap()
}

fn bytes_to_state(bytes: &Vec<u8>, state: &mut ProcessToVfs) {
    let serializable: ProcessToVfsSerializable = bincode::deserialize(&bytes).unwrap();
    for (process_id, vfs) in serializable.into_iter() {
        state.insert(process_id, Arc::new(Mutex::new(vfs)));
    }
}

async fn persist_state(our_node: String, send_to_loop: &MessageSender, state: &ProcessToVfs) {
    let _ = send_to_loop
        .send(KernelMessage {
            id: rand::random(),
            source: Address {
                node: our_node.clone(),
                process: ProcessId::Name("vfs".into()),
            },
            target: Address {
                node: our_node,
                process: ProcessId::Name("filesystem".into()),
            },
            rsvp: None,
            message: Message::Request(Request {
                inherit: true,
                expects_response: true,
                ipc: Some(serde_json::to_string(&FsAction::SetState).unwrap()),
                metadata: None,
            }),
            payload: Some(Payload {
                mime: None,
                bytes: state_to_bytes(state).await,
            }),
        })
        .await;
}

fn update_child_paths(
    parent_full_path: String,
    parent_new_full_path: String,
    key_to_entry: &mut KeyToEntry,
    path_to_key: &mut PathToKey,
) {
    let Some(parent_key) = path_to_key.remove(&parent_full_path) else {
        panic!("");
    };
    let Some(mut parent_entry) = key_to_entry.remove(&parent_key) else {
        panic!("");
    };
    let EntryType::Dir {
        parent: _,
        ref children,
    } = parent_entry.entry_type
    else {
        panic!("");
    };
    for child_key in children {
        let Some(mut child_entry) = key_to_entry.remove(&child_key) else {
            panic!("");
        };
        if !child_entry.full_path.starts_with(&parent_full_path) {
            panic!("");
        }
        let suffix = &child_entry.full_path[parent_full_path.len()..];
        let child_new_full_path = format!("{}{}", &parent_new_full_path, suffix);
        match child_entry.entry_type {
            EntryType::Dir {
                parent: _,
                children: _,
            } => {
                update_child_paths(
                    child_entry.full_path,
                    child_new_full_path,
                    key_to_entry,
                    path_to_key,
                );
            }
            EntryType::File { parent: _ } => {
                let (child_name, _) = make_file_name(&child_new_full_path);
                child_entry.name = child_name;
                child_entry.full_path = child_new_full_path.clone();
                key_to_entry.insert(child_key.clone(), child_entry);
                path_to_key.insert(child_new_full_path, child_key.clone());
            }
        }
    }
    let (parent_name, _) = make_dir_name(&parent_new_full_path);
    parent_entry.name = parent_name;
    parent_entry.full_path = parent_new_full_path.clone();
    key_to_entry.insert(parent_key.clone(), parent_entry);
    path_to_key.insert(parent_new_full_path, parent_key);
}

async fn load_state_from_reboot(
    our_node: String,
    send_to_loop: &MessageSender,
    mut recv_from_loop: &mut MessageReceiver,
    process_to_vfs: &mut ProcessToVfs,
) -> bool {
    let _ = send_to_loop
        .send(KernelMessage {
            id: rand::random(),
            source: Address {
                node: our_node.clone(),
                process: ProcessId::Name("vfs".into()),
            },
            target: Address {
                node: our_node.clone(),
                process: ProcessId::Name("filesystem".into()),
            },
            rsvp: None,
            message: Message::Request(Request {
                inherit: true,
                expects_response: true,
                ipc: Some(serde_json::to_string(&FsAction::GetState).unwrap()),
                metadata: None,
            }),
            payload: None,
        })
        .await;
    let km = recv_from_loop.recv().await;
    let Some(km) = km else {
        return false;
    };

    let KernelMessage {
        id: _,
        source: _,
        target: _,
        rsvp: _,
        message,
        payload,
    } = km;
    let Message::Response((Ok(Response { ipc, metadata: _ }), None)) = message else {
        return false;
    };
    let Some(ipc) = ipc else {
        panic!("");
    };
    let FsResponse::GetState = serde_json::from_str(&ipc).unwrap() else {
        panic!("");
    };
    let Some(payload) = payload else {
        panic!("");
    };
    bytes_to_state(&payload.bytes, process_to_vfs);

    return true;
}

fn build_state_for_initial_boot(
    process_map: &HashMap<ProcessId, (u128, OnPanic, HashSet<Capability>)>,
    process_to_vfs: &mut ProcessToVfs,
) {
    //  add wasm bytes to each process' vfs and to terminal's vfs
    let mut terminal_vfs = Vfs::new();
    for (process_id, (hash, _, _)) in process_map.iter() {
        let mut vfs = Vfs::new();
        let ProcessId::Name(process_name) = process_id else {
            process_to_vfs.insert(process_id.clone(), Arc::new(Mutex::new(vfs)));
            continue;
        };
        let name = format!("{}.wasm", process_name);
        let full_path = format!("/{}", name);
        let key = Key::File { id: hash.clone() };
        let entry_type = EntryType::File {
            parent: Key::Dir { id: 0 },
        };
        let entry = Entry {
            name,
            full_path: full_path.clone(),
            entry_type,
        };
        vfs.key_to_entry.insert(key.clone(), entry.clone());
        vfs.path_to_key.insert(full_path.clone(), key.clone());
        process_to_vfs.insert(process_id.clone(), Arc::new(Mutex::new(vfs)));

        terminal_vfs.key_to_entry.insert(key.clone(), entry);
        terminal_vfs.path_to_key.insert(full_path.clone(), key);
    }
    let terminal_process_id = ProcessId::Name("terminal".into());
    process_to_vfs.insert(terminal_process_id, Arc::new(Mutex::new(terminal_vfs)));
}

pub async fn vfs(
    our_node: String,
    process_map: HashMap<ProcessId, (u128, OnPanic, HashSet<Capability>)>,
    send_to_loop: MessageSender,
    send_to_terminal: PrintSender,
    mut recv_from_loop: MessageReceiver,
) -> anyhow::Result<()> {
    let mut process_to_vfs: ProcessToVfs = HashMap::new();
    let mut response_router: ResponseRouter = HashMap::new();
    let (send_vfs_task_done, mut recv_vfs_task_done): (
        tokio::sync::mpsc::Sender<u64>,
        tokio::sync::mpsc::Receiver<u64>,
    ) = tokio::sync::mpsc::channel(VFS_TASK_DONE_CHANNEL_CAPACITY);
    let (send_persist_state, mut recv_persist_state): (
        tokio::sync::mpsc::Sender<bool>,
        tokio::sync::mpsc::Receiver<bool>,
    ) = tokio::sync::mpsc::channel(VFS_PERSIST_STATE_CHANNEL_CAPACITY);

    let is_reboot = load_state_from_reboot(
        our_node.clone(),
        &send_to_loop,
        &mut recv_from_loop,
        &mut process_to_vfs,
    )
    .await;
    if !is_reboot {
        //  intial boot
        build_state_for_initial_boot(&process_map, &mut process_to_vfs);
        send_persist_state.send(true).await.unwrap();
    }

    loop {
        tokio::select! {
            id_done = recv_vfs_task_done.recv() => {
                let Some(id_done) = id_done else { continue };
                response_router.remove(&id_done);
            },
            _ = recv_persist_state.recv() => {
                persist_state(our_node.clone(), &send_to_loop, &process_to_vfs).await;
                continue;
            },
            km = recv_from_loop.recv() => {
                let Some(km) = km else { continue };
                if let Some(response_sender) = response_router.get(&km.id) {
                    response_sender.send(km).await.unwrap();
                    continue;
                }

                if our_node != km.source.node {
                    println!(
                        "vfs: request must come from our_node={}, got: {}",
                        our_node,
                        km.source.node,
                    );
                    continue;
                }
                let vfs = Arc::clone(match process_to_vfs.get(&km.source.process) {
                    Some(vfs) => vfs,
                    None => {
                        //  create open_files entry
                        process_to_vfs.insert(
                            km.source.process.clone(),
                            Arc::new(Mutex::new(Vfs::new())),
                        );
                        process_to_vfs.get(&km.source.process).unwrap()
                    }
                });
                //  TODO: remove after vfs is stable
                let _ = send_to_terminal.send(Printout {
                    verbosity: 1,
                    content: format!("{:?}", vfs)
                }).await;
                let our_node = our_node.clone();
                let source = km.source.clone();
                let id = km.id;

                let (response_sender, response_receiver): (
                    MessageSender,
                    MessageReceiver,
                ) = tokio::sync::mpsc::channel(VFS_RESPONSE_CHANNEL_CAPACITY);
                response_router.insert(id.clone(), response_sender);
                let send_to_loop = send_to_loop.clone();
                let send_persist_state = send_persist_state.clone();
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
                                send_persist_state,
                                send_to_terminal,
                                response_receiver,
                            ).await {
                                Err(e) => {
                                    send_to_loop
                                        .send(make_error_message(
                                            our_node.into(),
                                            id,
                                            source,
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
}

//  TODO: error handling: send error messages to caller
async fn handle_request(
    our_name: String,
    kernel_message: KernelMessage,
    vfs: Arc<Mutex<Vfs>>,
    send_to_loop: MessageSender,
    send_to_persist: tokio::sync::mpsc::Sender<bool>,
    send_to_terminal: PrintSender,
    recv_response: MessageReceiver,
) -> Result<(), VfsError> {
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
    }) = message
    else {
        panic!("");
        // return Err(FileSystemError::BadJson {
        //     json: "".into(),
        //     error: "not a Request with payload".into(),
        // });
    };

    let request: VfsRequest = match serde_json::from_str(&ipc) {
        Ok(r) => r,
        Err(e) => {
            panic!("{}", e);
            // return Err(FileSystemError::BadJson {
            //     json: ipc.into(),
            //     error: format!("parse failed: {:?}", e),
            // })
        }
    };

    let (ipc, bytes) = match_request(
        our_name.clone(),
        id.clone(),
        request,
        payload,
        vfs,
        &send_to_loop,
        &send_to_persist,
        &send_to_terminal,
        recv_response,
    )
    .await?;

    //  TODO: properly handle rsvp
    if expects_response {
        let response = KernelMessage {
            id: *id,
            source: Address {
                node: our_name.clone(),
                process: ProcessId::Name("vfs".into()),
            },
            target: Address {
                node: our_name.clone(),
                process: source.process.clone(),
            },
            rsvp,
            message: Message::Response((Ok(Response { ipc, metadata }), None)),
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

#[async_recursion::async_recursion]
async fn match_request(
    our_name: String,
    id: u64,
    request: VfsRequest,
    payload: Option<Payload>,
    vfs: Arc<Mutex<Vfs>>,
    send_to_loop: &MessageSender,
    send_to_persist: &tokio::sync::mpsc::Sender<bool>,
    send_to_terminal: &PrintSender,
    mut recv_response: MessageReceiver,
) -> Result<(Option<String>, Option<Vec<u8>>), VfsError> {
    Ok(match request {
        VfsRequest::Add {
            full_path,
            entry_type,
        } => {
            match entry_type {
                AddEntryType::Dir => {
                    if let Some(last_char) = full_path.chars().last() {
                        if last_char != '/' {
                            //  TODO: panic or correct & notify?
                            //  elsewhere we panic
                            // format!("{}/", full_path)
                            send_to_terminal
                                .send(Printout {
                                    verbosity: 0,
                                    content: format!(
                                        "vfs: cannot add dir without trailing `/`: {}",
                                        full_path
                                    ),
                                })
                                .await
                                .unwrap();
                            panic!("");
                        };
                    } else {
                        panic!("empty path");
                    };
                    let mut vfs = vfs.lock().await;
                    if vfs.path_to_key.contains_key(&full_path) {
                        send_to_terminal
                            .send(Printout {
                                verbosity: 0,
                                content: format!("vfs: not overwriting dir {}", full_path),
                            })
                            .await
                            .unwrap();
                        panic!(""); //  TODO: error?
                    };
                    let (name, parent_path) = make_dir_name(&full_path);
                    let Some(parent_key) = vfs.path_to_key.remove(&parent_path) else {
                        panic!("fp, pp: {}, {}", full_path, parent_path);
                    };
                    let key = Key::Dir { id: rand::random() };
                    vfs.key_to_entry.insert(
                        key.clone(),
                        Entry {
                            name,
                            full_path: full_path.clone(),
                            entry_type: EntryType::Dir {
                                parent: parent_key.clone(),
                                children: HashSet::new(),
                            },
                        },
                    );
                    vfs.path_to_key.insert(parent_path, parent_key);
                    vfs.path_to_key.insert(full_path.clone(), key.clone());
                }
                AddEntryType::NewFile => {
                    if let Some(last_char) = full_path.chars().last() {
                        if last_char == '/' {
                            send_to_terminal
                                .send(Printout {
                                    verbosity: 0,
                                    content: format!(
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
                    let mut vfs = vfs.lock().await;
                    if vfs.path_to_key.contains_key(&full_path) {
                        send_to_terminal
                            .send(Printout {
                                verbosity: 1,
                                content: format!("vfs: overwriting file {}", full_path),
                            })
                            .await
                            .unwrap();
                        let Some(old_key) = vfs.path_to_key.remove(&full_path) else {
                            panic!("");
                        };
                        vfs.key_to_entry.remove(&old_key);
                    };

                    let _ = send_to_loop
                        .send(KernelMessage {
                            id,
                            source: Address {
                                node: our_name.clone(),
                                process: ProcessId::Name("vfs".into()),
                            },
                            target: Address {
                                node: our_name.clone(),
                                process: ProcessId::Name("filesystem".into()),
                            },
                            rsvp: None,
                            message: Message::Request(Request {
                                inherit: true,
                                expects_response: true,
                                ipc: Some(serde_json::to_string(&FsAction::Write).unwrap()),
                                metadata: None,
                            }),
                            payload,
                        })
                        .await;
                    let write_response = recv_response.recv().await.unwrap();
                    let KernelMessage {
                        id: _,
                        source: _,
                        target: _,
                        rsvp: _,
                        message,
                        payload: _,
                    } = write_response;
                    let Message::Response((Ok(Response { ipc, metadata: _ }), None)) = message
                    else {
                        panic!("")
                    };
                    let Some(ipc) = ipc else {
                        panic!("");
                    };
                    let FsResponse::Write(hash) = serde_json::from_str(&ipc).unwrap() else {
                        panic!("");
                    };

                    let (name, parent_path) = make_file_name(&full_path);
                    let Some(parent_key) = vfs.path_to_key.remove(&parent_path) else {
                        panic!("");
                    };
                    let key = Key::File { id: hash };
                    vfs.key_to_entry.insert(
                        key.clone(),
                        Entry {
                            name,
                            full_path: full_path.clone(),
                            entry_type: EntryType::File {
                                parent: parent_key.clone(),
                            },
                        },
                    );
                    vfs.path_to_key.insert(parent_path, parent_key);
                    vfs.path_to_key.insert(full_path.clone(), key.clone());
                }
                AddEntryType::ExistingFile { hash } => {
                    if let Some(last_char) = full_path.chars().last() {
                        if last_char == '/' {
                            send_to_terminal
                                .send(Printout {
                                    verbosity: 0,
                                    content: format!(
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
                    let mut vfs = vfs.lock().await;
                    if vfs.path_to_key.contains_key(&full_path) {
                        send_to_terminal
                            .send(Printout {
                                verbosity: 1,
                                content: format!("vfs: overwriting file {}", full_path),
                            })
                            .await
                            .unwrap();
                        let Some(old_key) = vfs.path_to_key.remove(&full_path) else {
                            panic!("no old key");
                        };
                        vfs.key_to_entry.remove(&old_key);
                    };
                    let (name, parent_path) = make_file_name(&full_path);
                    let Some(parent_key) = vfs.path_to_key.remove(&parent_path) else {
                        panic!("");
                    };
                    let key = Key::File { id: hash };
                    vfs.key_to_entry.insert(
                        key.clone(),
                        Entry {
                            name,
                            full_path: full_path.clone(),
                            entry_type: EntryType::File {
                                parent: parent_key.clone(),
                            },
                        },
                    );
                    vfs.path_to_key.insert(parent_path, parent_key);
                    vfs.path_to_key.insert(full_path.clone(), key.clone());
                }
            }
            send_to_persist.send(true).await.unwrap();
            (
                Some(
                    serde_json::to_string(&VfsResponse::Add {
                        full_path: full_path.clone(),
                    })
                    .unwrap(),
                ),
                None,
            )
        }
        VfsRequest::Rename {
            full_path,
            new_full_path,
        } => {
            let mut vfs = vfs.lock().await;
            let Some(key) = vfs.path_to_key.remove(&full_path) else {
                send_to_terminal
                    .send(Printout {
                        verbosity: 0,
                        content: format!("vfs: can't rename: nonexistent file {}", full_path),
                    })
                    .await
                    .unwrap();
                panic!("");
            };
            let Some(mut entry) = vfs.key_to_entry.remove(&key) else {
                send_to_terminal
                    .send(Printout {
                        verbosity: 0,
                        content: format!("vfs: can't rename: nonexistent file {}", full_path),
                    })
                    .await
                    .unwrap();
                panic!("");
            };
            match entry.entry_type {
                EntryType::Dir {
                    parent: _,
                    ref children,
                } => {
                    if vfs.path_to_key.contains_key(&new_full_path) {
                        send_to_terminal
                            .send(Printout {
                                verbosity: 0,
                                content: format!("vfs: not overwriting dir {}", new_full_path),
                            })
                            .await
                            .unwrap();
                        vfs.path_to_key.insert(full_path, key);
                        panic!(""); //  TODO: error?
                    };
                    let (name, _) = make_dir_name(&new_full_path);
                    entry.name = name;
                    entry.full_path = new_full_path.clone();
                    vfs.path_to_key.insert(new_full_path.clone(), key.clone());
                    vfs.key_to_entry.insert(key, entry);
                    //  TODO: recursively apply path update to all children
                    //  update_child_paths(full_path, new_full_path, children);
                }
                EntryType::File { parent: _ } => {
                    if vfs.path_to_key.contains_key(&new_full_path) {
                        send_to_terminal
                            .send(Printout {
                                verbosity: 1,
                                content: format!("vfs: overwriting file {}", new_full_path),
                            })
                            .await
                            .unwrap();
                    };
                    let (name, _) = make_file_name(&new_full_path);
                    entry.name = name;
                    entry.full_path = new_full_path.clone();
                    vfs.path_to_key.insert(new_full_path.clone(), key.clone());
                    vfs.key_to_entry.insert(key, entry);
                }
            }
            send_to_persist.send(true).await.unwrap();
            (
                Some(serde_json::to_string(&VfsResponse::Rename { new_full_path }).unwrap()),
                None,
            )
        }
        VfsRequest::Delete { full_path } => {
            let mut vfs = vfs.lock().await;
            let Some(key) = vfs.path_to_key.remove(&full_path) else {
                send_to_terminal
                    .send(Printout {
                        verbosity: 0,
                        content: format!("vfs: can't delete: nonexistent entry {}", full_path),
                    })
                    .await
                    .unwrap();
                panic!("");
            };
            let Some(entry) = vfs.key_to_entry.remove(&key) else {
                send_to_terminal
                    .send(Printout {
                        verbosity: 0,
                        content: format!("vfs: can't delete: nonexistent entry {}", full_path),
                    })
                    .await
                    .unwrap();
                panic!("");
            };
            match entry.entry_type {
                EntryType::Dir {
                    parent: _,
                    ref children,
                } => {
                    if !children.is_empty() {
                        send_to_terminal
                            .send(Printout {
                                verbosity: 0,
                                content: format!(
                                    "vfs: can't delete: non-empty directory {}",
                                    full_path
                                ),
                            })
                            .await
                            .unwrap();
                        vfs.path_to_key.insert(full_path.clone(), key.clone());
                        vfs.key_to_entry.insert(key.clone(), entry);
                    }
                }
                EntryType::File { parent } => {
                    match vfs.key_to_entry.get_mut(&parent) {
                        None => {
                            send_to_terminal
                                .send(Printout {
                                    verbosity: 0,
                                    content: format!(
                                        "vfs: delete: unexpected file with no parent dir: {}",
                                        full_path
                                    ),
                                })
                                .await
                                .unwrap();
                            panic!("");
                        }
                        Some(parent) => {
                            let EntryType::Dir {
                                parent: _,
                                ref mut children,
                            } = parent.entry_type
                            else {
                                panic!("");
                            };
                            //  TODO: does this work?
                            children.remove(&key);
                        }
                    }
                }
            }
            send_to_persist.send(true).await.unwrap();
            (
                Some(serde_json::to_string(&VfsResponse::Delete { full_path }).unwrap()),
                None,
            )
        }
        VfsRequest::GetPath { hash } => {
            let mut vfs = vfs.lock().await;
            let key = Key::File { id: hash.clone() };
            let ipc = Some(
                serde_json::to_string(&VfsResponse::GetPath {
                    hash,
                    full_path: match vfs.key_to_entry.remove(&key) {
                        None => None,
                        Some(entry) => {
                            let full_path = entry.full_path.clone();
                            vfs.key_to_entry.insert(key, entry);
                            Some(full_path)
                        }
                    },
                })
                .unwrap(),
            );
            (ipc, None)
        }
        VfsRequest::GetEntry { ref full_path } => {
            let (key, entry, paths) = {
                let mut vfs = vfs.lock().await;
                let key = vfs.path_to_key.remove(full_path);
                match key {
                    None => (None, None, vec![]),
                    Some(key) => {
                        vfs.path_to_key.insert(full_path.clone(), key.clone());
                        let entry = vfs.key_to_entry.remove(&key);
                        match entry {
                            None => (Some(key), None, vec![]),
                            Some(ref e) => {
                                vfs.key_to_entry.insert(key.clone(), e.clone());
                                match e.entry_type {
                                    EntryType::File { parent: _ } => (Some(key), entry, vec![]),
                                    EntryType::Dir {
                                        parent: _,
                                        ref children,
                                    } => {
                                        let mut paths: Vec<String> = Vec::new();
                                        for child in children {
                                            let Some(child) = vfs.key_to_entry.get(&child) else {
                                                send_to_terminal
                                                    .send(Printout {
                                                        verbosity: 0,
                                                        content: format!(
                                                            "vfs: child missing for: {}",
                                                            full_path
                                                        ),
                                                    })
                                                    .await
                                                    .unwrap();
                                                continue;
                                            };
                                            paths.push(child.full_path.clone());
                                        }
                                        paths.sort();
                                        (Some(key), entry, paths)
                                    }
                                }
                            }
                        }
                    }
                }
            };

            let entry_not_found = (
                Some(
                    serde_json::to_string(&VfsResponse::GetEntry {
                        full_path: full_path.clone(),
                        children: vec![],
                    })
                    .unwrap(),
                ),
                None,
            );
            match key {
                None => entry_not_found,
                Some(key) => match entry {
                    None => entry_not_found,
                    Some(entry) => match entry.entry_type {
                        EntryType::Dir {
                            parent: _,
                            children: _,
                        } => (
                            Some(
                                serde_json::to_string(&VfsResponse::GetEntry {
                                    full_path: full_path.clone(),
                                    children: paths,
                                })
                                .unwrap(),
                            ),
                            None,
                        ),
                        EntryType::File { parent: _ } => {
                            let Key::File { id: file_hash } = key else {
                                panic!("");
                            };
                            let _ = send_to_loop
                                .send(KernelMessage {
                                    id,
                                    source: Address {
                                        node: our_name.clone(),
                                        process: ProcessId::Name("vfs".into()),
                                    },
                                    target: Address {
                                        node: our_name.clone(),
                                        process: ProcessId::Name("filesystem".into()),
                                    },
                                    rsvp: None,
                                    message: Message::Request(Request {
                                        inherit: true,
                                        expects_response: true,
                                        ipc: Some(
                                            serde_json::to_string(&FsAction::Read(
                                                file_hash.clone(),
                                            ))
                                            .unwrap(),
                                        ),
                                        metadata: None,
                                    }),
                                    payload: None,
                                })
                                .await;
                            let read_response = recv_response.recv().await.unwrap();
                            let KernelMessage {
                                id: _,
                                source: _,
                                target: _,
                                rsvp: _,
                                message,
                                payload,
                            } = read_response;
                            let Message::Response((Ok(Response { ipc, metadata: _ }), None)) =
                                message
                            else {
                                panic!("")
                            };
                            let Some(ipc) = ipc else {
                                panic!("");
                            };
                            let FsResponse::Read(read_hash) = serde_json::from_str(&ipc).unwrap()
                            else {
                                panic!("");
                            };
                            assert_eq!(file_hash, read_hash);
                            let Some(payload) = payload else {
                                panic!("");
                            };
                            (
                                Some(
                                    serde_json::to_string(&VfsResponse::GetEntry {
                                        full_path: full_path.clone(),
                                        children: vec![],
                                    })
                                    .unwrap(),
                                ),
                                Some(payload.bytes),
                            )
                        }
                    },
                },
            }
        }
        VfsRequest::GetFileChunk {
            full_path,
            offset,
            length,
        } => {
            let file_hash = {
                let mut vfs = vfs.lock().await;
                let Some(key) = vfs.path_to_key.remove(&full_path) else {
                    panic!(""); //  TODO
                };
                let key2 = key.clone();
                let Key::File { id: file_hash } = key2 else {
                    panic!(""); //  TODO
                };
                vfs.path_to_key.insert(full_path.clone(), key);
                file_hash
            };

            let _ = send_to_loop
                .send(KernelMessage {
                    id,
                    source: Address {
                        node: our_name.clone(),
                        process: ProcessId::Name("vfs".into()),
                    },
                    target: Address {
                        node: our_name.clone(),
                        process: ProcessId::Name("filesystem".into()),
                    },
                    rsvp: None,
                    message: Message::Request(Request {
                        inherit: true,
                        expects_response: true,
                        ipc: Some(
                            serde_json::to_string(&FsAction::ReadChunk(ReadChunkRequest {
                                file: file_hash.clone(),
                                start: offset,
                                length,
                            }))
                            .unwrap(),
                        ),
                        metadata: None,
                    }),
                    payload: None,
                })
                .await;
            let read_response = recv_response.recv().await.unwrap();
            let KernelMessage {
                id: _,
                source: _,
                target: _,
                rsvp: _,
                message,
                payload,
            } = read_response;
            let Message::Response((Ok(Response { ipc, metadata: _ }), None)) = message else {
                panic!("")
            };
            let Some(ipc) = ipc else {
                panic!("");
            };
            let FsResponse::ReadChunk(read_hash) = serde_json::from_str(&ipc).unwrap() else {
                panic!("");
            };
            assert_eq!(file_hash, read_hash);
            let Some(payload) = payload else {
                panic!("");
            };

            (
                Some(
                    serde_json::to_string(&VfsResponse::GetFileChunk {
                        full_path,
                        offset,
                        length,
                    })
                    .unwrap(),
                ),
                Some(payload.bytes),
            )
        }
        VfsRequest::WriteOffset { full_path, offset } => {
            let file_hash = {
                let mut vfs = vfs.lock().await;
                let Some(key) = vfs.path_to_key.remove(&full_path) else {
                    panic!("");
                };
                let key2 = key.clone();
                let Key::File { id: file_hash } = key2 else {
                    panic!(""); //  TODO
                };
                vfs.path_to_key.insert(full_path.clone(), key);
                file_hash
            };
            let _ = send_to_loop
                .send(KernelMessage {
                    id,
                    source: Address {
                        node: our_name.clone(),
                        process: ProcessId::Name("vfs".into()),
                    },
                    target: Address {
                        node: our_name.clone(),
                        process: ProcessId::Name("filesystem".into()),
                    },
                    rsvp: None,
                    message: Message::Request(Request {
                        inherit: true,
                        expects_response: true,
                        ipc: Some(
                            serde_json::to_string(&FsAction::WriteOffset((file_hash, offset)))
                                .unwrap(),
                        ),
                        metadata: None,
                    }),
                    payload,
                })
                .await;

            (
                Some(
                    serde_json::to_string(&VfsResponse::WriteOffset { full_path, offset }).unwrap(),
                ),
                None,
            )
        }
        VfsRequest::GetEntryLength { full_path } => {
            if full_path.chars().last() == Some('/') {
                (
                    Some(
                        serde_json::to_string(&VfsResponse::GetEntryLength {
                            full_path,
                            length: 0,
                        })
                        .unwrap(),
                    ),
                    None,
                )
            } else {
                let file_hash = {
                    let mut vfs = vfs.lock().await;
                    let Some(key) = vfs.path_to_key.remove(&full_path) else {
                        panic!("");
                    };
                    let key2 = key.clone();
                    let Key::File { id: file_hash } = key2 else {
                        panic!(""); //  TODO
                    };
                    vfs.path_to_key.insert(full_path.clone(), key);
                    file_hash
                };

                let _ = send_to_loop
                    .send(KernelMessage {
                        id,
                        source: Address {
                            node: our_name.clone(),
                            process: ProcessId::Name("vfs".into()),
                        },
                        target: Address {
                            node: our_name.clone(),
                            process: ProcessId::Name("filesystem".into()),
                        },
                        rsvp: None,
                        message: Message::Request(Request {
                            inherit: true,
                            expects_response: true,
                            ipc: Some(serde_json::to_string(&FsAction::Length(file_hash)).unwrap()),
                            metadata: None,
                        }),
                        payload: None,
                    })
                    .await;
                let length_response = recv_response.recv().await.unwrap();
                let KernelMessage {
                    id: _,
                    source: _,
                    target: _,
                    rsvp: _,
                    message,
                    payload: _,
                } = length_response;
                let Message::Response((Ok(Response { ipc, metadata: _ }), None)) = message else {
                    panic!("")
                };
                let Some(ipc) = ipc else {
                    panic!("");
                };
                let FsResponse::Length(length) = serde_json::from_str(&ipc).unwrap() else {
                    panic!("");
                };

                (
                    Some(
                        serde_json::to_string(&VfsResponse::GetEntryLength { full_path, length })
                            .unwrap(),
                    ),
                    None,
                )
            }
        }
    })
}
