use std::collections::HashMap;
use serde::{Serialize, Deserialize};
use bindings::print_to_terminal;
use bindings::component::microkernel_process::types::WitMessageType;
use bindings::component::microkernel_process::types::WitPayload;
use bindings::component::microkernel_process::types::WitProtomessageType;
use bindings::component::microkernel_process::types::WitRequestTypeWithTarget;

struct Component;

#[derive(Debug, Serialize, Deserialize)]
pub struct FileSystemRequest {
    pub uri_string: String,
    pub action: FileSystemAction,
}
#[derive(Debug, Serialize, Deserialize)]
pub enum FileSystemAction {
    Read,
    Write,
    GetMetadata,
    OpenRead,
    OpenAppend,
    Append,
    ReadChunkFromOpen(u64),
    SeekWithinOpen(FileSystemSeekFrom),
}
//  copy of std::io::SeekFrom with Serialize/Deserialize
#[derive(Debug, Serialize, Deserialize)]
pub enum FileSystemSeekFrom {
    Start(u64),
    End(i64),
    Current(i64),
}
#[derive(Debug, Serialize, Deserialize)]
pub enum FileSystemResponse {
    Read(FileSystemUriHash),
    Write(String),
    GetMetadata(FileSystemMetadata),
    OpenRead(String),
    OpenAppend(String),
    Append(String),
    ReadChunkFromOpen(FileSystemUriHash),
    SeekWithinOpen(String),
}
#[derive(Debug, Serialize, Deserialize)]
pub struct FileSystemUriHash {
    pub uri_string: String,
    pub hash: u64,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct FileSystemMetadata {
    pub uri_string: String,
    pub hash: u64,
    pub is_dir: bool,
    pub is_file: bool,
    pub is_symlink: bool,
    pub len: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, Hash, PartialEq)]
struct FileTransferKey {
    requester: String,
    server: String,
    uri_string: String,
}
#[derive(Debug, Serialize, Deserialize)]
struct FileTransferGetFile {
    target_ship: String,
    uri_string: String,
    chunk_size: u64,
}
#[derive(Debug, Serialize, Deserialize)]
struct FileTransferStart {
    uri_string: String,
    chunk_size: u64,
}
#[derive(Debug, Serialize, Deserialize)]
struct FileTransferGetPiece {
    uri_string: String,
    chunk_size: u64,
    piece_number: u32,
}
#[derive(Debug, Serialize, Deserialize)]
struct FileTransferCancel {
    uri_string: String,
}
#[derive(Debug, Serialize, Deserialize)]
struct FileTransferReadyToReceive {
    uri_string: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum FileTransferRequest {
    //  actions
    GetFile(FileTransferGetFile),
    Start(FileTransferStart),
    Cancel(FileTransferCancel),
    GetPiece(FileTransferGetPiece),
    //  updates
    FilePiece(FileTransferFilePiece),
    ReadyToReceive(FileTransferReadyToReceive),
}
 
#[derive(Debug, Serialize, Deserialize)]
struct FileTransferFilePiece {
    uri_string: String,
    piece_number: u32,
    piece_hash: u64,
}
 
#[derive(Clone, Debug, Serialize, Deserialize)]
struct FileTransferMetadata {
    key: FileTransferKey,
    hash: u64,
    chunk_size: u64,
    number_pieces: u32,
    number_bytes: u64,
    // piece_hashes: Vec<u64>,  //  ?
}
#[derive(Debug, Serialize, Deserialize)]
struct FileTransferPieceReceived {
    uri_string: String,
    piece_number: u32,
}
#[derive(Debug, Serialize, Deserialize)]
struct FileTransferError {
    uri_string: String,
    error: String,
}
#[derive(Debug, Serialize, Deserialize)]
enum FileTransferResponse {
    Started(FileTransferMetadata),
    PieceReceived(FileTransferPieceReceived),
    FileReceived(String),  //  uri_string to derive key
    Errored(FileTransferError),
}

struct IsConfirmedPiece {
    piece_hash: u64,
    is_confirmed: bool,
}
struct Downloading {
    metadata: FileTransferMetadata,
    received_pieces: Vec<u64>,  //  piece_hash
}
struct Uploading {
    metadata: FileTransferMetadata,
    sent_pieces: Vec<IsConfirmedPiece>,
}

#[derive(Debug, Serialize, Deserialize)]
struct FileSystemMetadataContext {
    node: String,
    chunk_size: u64,
}
#[derive(Debug, Serialize, Deserialize)]
struct FileTransferPieceContext {
    node: String,
    piece_number: u32,
}
#[derive(Debug, Serialize, Deserialize)]
struct FileTransferNodeContext {
    node: String,
}

fn div_round_up(numerator: u64, denominator: u64) -> u64 {
    (numerator + denominator - 1) / denominator
}

impl bindings::MicrokernelProcess for Component {
    fn run_process(our_name: String, process_name: String) {
        print_to_terminal("file_transfer: begin");

        //  in progress
        let mut downloads: HashMap<FileTransferKey, Downloading> = HashMap::new();
        let mut uploads: HashMap<FileTransferKey, Uploading> = HashMap::new();

        loop {
            let (message, context) = bindings::await_next_message();
            let Some(ref payload_json_string) = message.payload.json else {
                panic!("foo")
            };

            print_to_terminal(format!("{}: got json {}", process_name, payload_json_string).as_str());

            match message.message_type {
                WitMessageType::Request(_is_expecting_response) => {
                    //  TODO: perms;
                    //   only GetFile and Cancel allowed from non file_transfer
                    //   and Cancel should probably only be allowed from same
                    //   process as GetFile came from
                    print_to_terminal("Request");
                    let request: FileTransferRequest =
                        match serde_json::from_str(payload_json_string) {
                            Ok(result) => result,
                            Err(e) => {
                                print_to_terminal(format!("couldnt parse json string: {}", e).as_str());
                                continue;
                            },
                        };
                    match request {
                        FileTransferRequest::GetFile(get_file) => {
                            print_to_terminal("GetFile");

                            bindings::yield_results(vec![
                                (
                                    bindings::WitProtomessage {
                                        protomessage_type: WitProtomessageType::Request(
                                            WitRequestTypeWithTarget {
                                                is_expecting_response: true,
                                                target_ship: &get_file.target_ship,
                                                target_app: &process_name,
                                            },
                                        ),
                                        payload: &WitPayload {
                                            json: Some(serde_json::to_string(
                                                &FileTransferRequest::Start(
                                                    FileTransferStart {
                                                        uri_string: get_file.uri_string,
                                                        chunk_size: get_file.chunk_size,
                                                    }
                                                )
                                            ).unwrap()),
                                            bytes: None,
                                        },
                                    },
                                    "",
                                )
                            ].as_slice());
                        },
                        FileTransferRequest::Start(start) => {
                            print_to_terminal("Start");

                            let context = serde_json::to_string(&FileSystemMetadataContext {
                                node: message.wire.source_ship,
                                chunk_size: start.chunk_size,
                            }).unwrap();
                            bindings::yield_results(vec![
                                (
                                    bindings::WitProtomessage {
                                        protomessage_type: WitProtomessageType::Request(
                                            WitRequestTypeWithTarget {
                                                is_expecting_response: true,
                                                target_ship: &our_name,
                                                target_app: "filesystem",
                                            },
                                        ),
                                        payload: &WitPayload {
                                            json: Some(serde_json::to_string(
                                                &FileSystemRequest {
                                                    uri_string: start.uri_string,
                                                    action: FileSystemAction::GetMetadata,
                                                }
                                            ).unwrap()),
                                            bytes: None,
                                        },
                                    },
                                    context.as_str(),
                                ),
                            ].as_slice());
                        },
                        FileTransferRequest::Cancel(_uri_string) => {
                            panic!("cancel: todo");
                        },
                        FileTransferRequest::GetPiece(_get_piece) => {
                            panic!("cancel: todo");
                        },
                        FileTransferRequest::FilePiece(file_piece) => {
                            print_to_terminal("FilePiece");

                            let Some(bytes) = message.payload.bytes.clone() else {
                                panic!("bytes");
                            };
                            let key = FileTransferKey {
                                requester: our_name.clone(),
                                server: message.wire.source_ship.clone(),
                                uri_string: file_piece.uri_string.clone(),
                            };

                            let downloading = downloads.get_mut(&key).unwrap();
                            if downloading.received_pieces.len() != file_piece.piece_number as usize {
                                panic!("FilePiece");
                            }
                            downloading.received_pieces.push(file_piece.piece_hash);

                            let context = serde_json::to_string(&FileTransferPieceContext {
                                node: message.wire.source_ship.clone(),
                                piece_number: file_piece.piece_number,
                            }).unwrap();
                            bindings::yield_results(vec![
                                (
                                    bindings::WitProtomessage {
                                        protomessage_type: WitProtomessageType::Request(
                                            WitRequestTypeWithTarget {
                                                is_expecting_response: true,
                                                target_ship: &our_name,
                                                target_app: "filesystem",
                                            },
                                        ),
                                        payload: &WitPayload {
                                            json: Some(serde_json::to_string(
                                                &FileSystemRequest {
                                                    uri_string: file_piece.uri_string,
                                                    action: FileSystemAction::Append,
                                                }
                                            ).unwrap()),
                                            bytes: Some(bytes),
                                        },
                                    },
                                    context.as_str(),
                                ),
                            ].as_slice());
                        },
                        FileTransferRequest::ReadyToReceive(ready_to_receive) => {
                            print_to_terminal("ReadyToReceive");

                            let key = FileTransferKey {
                                requester: message.wire.source_ship.clone(),
                                server: our_name.clone(),
                                uri_string: ready_to_receive.uri_string.clone(),
                            };

                            let uploading = uploads.get(&key).unwrap();

                            let context = serde_json::to_string(&FileTransferNodeContext {
                                node: message.wire.source_ship.clone(),
                            }).unwrap();
                            bindings::yield_results(vec![
                                (
                                    bindings::WitProtomessage {
                                        protomessage_type: WitProtomessageType::Request(
                                            WitRequestTypeWithTarget {
                                                is_expecting_response: true,
                                                target_ship: &our_name,
                                                target_app: "filesystem",
                                            },
                                        ),
                                        payload: &WitPayload {
                                            json: Some(serde_json::to_string(
                                                &FileSystemRequest {
                                                    uri_string: ready_to_receive.uri_string,
                                                    action: FileSystemAction::ReadChunkFromOpen(
                                                        uploading.metadata.chunk_size,
                                                    ),
                                                }
                                            ).unwrap()),
                                            bytes: None,
                                        },
                                    },
                                    context.as_str(),
                                )
                            ].as_slice());
                        },
                    }
                },
                WitMessageType::Response => {
                    print_to_terminal("Response");

                    if "filesystem" == message.wire.source_app {
                        let response: FileSystemResponse = serde_json::from_str(payload_json_string).unwrap();
                        match response {
                            FileSystemResponse::GetMetadata(file_metadata) => {
                                print_to_terminal("GetMetadata");

                                let context: FileSystemMetadataContext =
                                    serde_json::from_str(&context).unwrap();

                                let key = FileTransferKey {
                                    requester: context.node.clone(),
                                    server: our_name.clone(),
                                    uri_string: file_metadata.uri_string.clone(),
                                };
                                let number_pieces = div_round_up(
                                    file_metadata.len,
                                    context.chunk_size
                                ) as u32;
                                let metadata = FileTransferMetadata {
                                    key: key.clone(),
                                    hash: file_metadata.hash,
                                    chunk_size: context.chunk_size,
                                    number_pieces,
                                    number_bytes: file_metadata.len,
                                };
                                uploads.insert(
                                    key,
                                    Uploading {
                                        metadata: metadata.clone(),
                                        sent_pieces: vec![],
                                    }
                                );

                                let context = serde_json::to_string(&FileTransferNodeContext {
                                    node: context.node,
                                }).unwrap();
                                bindings::yield_results(vec![
                                    (
                                        bindings::WitProtomessage {
                                            protomessage_type: WitProtomessageType::Response,
                                            payload: &WitPayload {
                                                json: Some(serde_json::to_string(
                                                    &FileTransferResponse::Started(metadata)
                                                ).unwrap()),
                                                bytes: None,
                                            },
                                        },
                                        "",
                                    ),
                                    (
                                        bindings::WitProtomessage {
                                            protomessage_type: WitProtomessageType::Request(
                                                WitRequestTypeWithTarget {
                                                    is_expecting_response: true,
                                                    target_ship: &our_name,
                                                    target_app: "filesystem",
                                                },
                                            ),
                                            payload: &WitPayload {
                                                json: Some(serde_json::to_string(
                                                    &FileSystemRequest {
                                                        uri_string: file_metadata.uri_string,
                                                        action: FileSystemAction::OpenRead,
                                                    }
                                                ).unwrap()),
                                                bytes: None,
                                            },
                                        },
                                        context.as_str(),
                                    ),
                                ].as_slice());
                            },
                            FileSystemResponse::OpenRead(_uri_string) => {
                                print_to_terminal("Successfully opened Read");
                            },
                            FileSystemResponse::OpenAppend(uri_string) => {
                                print_to_terminal("OpenAppend");

                                let context: FileTransferNodeContext = serde_json::from_str(&context).unwrap();
                                bindings::yield_results(vec![
                                    (
                                        bindings::WitProtomessage {
                                            protomessage_type: WitProtomessageType::Request(
                                                WitRequestTypeWithTarget {
                                                    is_expecting_response: false,
                                                    target_ship: &context.node,
                                                    target_app: &process_name,
                                                },
                                            ),
                                            payload: &WitPayload {
                                                json: Some(serde_json::to_string(
                                                    &FileTransferRequest::ReadyToReceive(
                                                        FileTransferReadyToReceive {
                                                            uri_string,
                                                        }
                                                    )
                                                ).unwrap()),
                                                bytes: None,
                                            },
                                        },
                                        "",
                                    )
                                ].as_slice());
                                print_to_terminal("OpenAppend 2");
                            },
                            FileSystemResponse::ReadChunkFromOpen(uri_hash) => {
                                print_to_terminal("ReadChunkFromOpen");

                                let context: FileTransferNodeContext = serde_json::from_str(&context).unwrap();
                                let Some(bytes) = message.payload.bytes.clone() else {
                                    panic!("bytes");
                                };

                                let key = FileTransferKey {
                                    requester: context.node.clone(),
                                    server: our_name.clone(),
                                    uri_string: uri_hash.uri_string.clone(),
                                };

                                let uploading = uploads.get_mut(&key).unwrap();
                                uploading.sent_pieces.push(IsConfirmedPiece {
                                    piece_hash: uri_hash.hash.clone(),
                                    is_confirmed: false,
                                });

                                bindings::yield_results(vec![
                                    (
                                        bindings::WitProtomessage {
                                            protomessage_type: WitProtomessageType::Request(
                                                WitRequestTypeWithTarget {
                                                    is_expecting_response: true,
                                                    target_ship: &context.node,
                                                    target_app: "file_transfer",
                                                },
                                            ),
                                            payload: &WitPayload {
                                                json: Some(serde_json::to_string(
                                                    &FileTransferRequest::FilePiece(
                                                        FileTransferFilePiece {
                                                            uri_string: uri_hash.uri_string,
                                                            piece_number: (uploading.sent_pieces.len() - 1) as u32,
                                                            piece_hash: uri_hash.hash,
                                                        }
                                                    )
                                                ).unwrap()),
                                                bytes: Some(bytes),
                                            },
                                        },
                                        "",
                                    )
                                ].as_slice());
                            },
                            FileSystemResponse::Append(uri_string) => {
                                print_to_terminal("Append");
                                
                                let context: FileTransferPieceContext =
                                    serde_json::from_str(&context).unwrap();

                                let key = FileTransferKey {
                                    requester: our_name.clone(),
                                    server: context.node,
                                    uri_string: uri_string.clone(),
                                };

                                let downloading = downloads.remove(&key).unwrap();
                                if downloading.received_pieces.len() == downloading.metadata.number_pieces as usize {
                                    //  received all file pieces
                                    bindings::yield_results(vec![
                                        (
                                            bindings::WitProtomessage {
                                                protomessage_type: WitProtomessageType::Response,
                                                payload: &WitPayload {
                                                    json: Some(serde_json::to_string(
                                                        &FileTransferResponse::FileReceived(
                                                            uri_string.clone(),
                                                        )
                                                    ).unwrap()),
                                                    bytes: None,
                                                },
                                            },
                                            "",
                                        )
                                    ].as_slice());
                                } else {
                                    //  still expecting file pieces
                                    downloads.insert(key, downloading);

                                    bindings::yield_results(vec![
                                        (
                                            bindings::WitProtomessage {
                                                protomessage_type: WitProtomessageType::Response,
                                                payload: &WitPayload {
                                                    json: Some(serde_json::to_string(
                                                        &FileTransferResponse::PieceReceived(
                                                            FileTransferPieceReceived {
                                                                uri_string: uri_string,
                                                                piece_number: context.piece_number,
                                                            }
                                                        )
                                                    ).unwrap()),
                                                    bytes: None,
                                                },
                                            },
                                            "",
                                        )
                                    ].as_slice());
                                }
                            },
                            _ => {
                                panic!("bar");
                            },
                        }
                    } else if process_name == message.wire.source_app {
                        let response: FileTransferResponse =
                            serde_json::from_str(payload_json_string).unwrap();
                        match response {
                            FileTransferResponse::Started(metadata) => {
                                print_to_terminal("Started");

                                let uri_string = metadata.key.uri_string.clone();
                                let key = FileTransferKey {
                                    requester: our_name.clone(),
                                    server: message.wire.source_ship.clone(),
                                    uri_string: uri_string.clone(),
                                };
                                if metadata.key != key {
                                    panic!("started");
                                }
                                downloads.insert(
                                    key,
                                    Downloading {
                                        metadata,
                                        received_pieces: vec![],
                                    }
                                );
                                print_to_terminal("Started 6");
                                print_to_terminal(
                                    format!(
                                        "Started downloads keys: {:?}",
                                        downloads.keys().collect::<Vec<_>>(),
                                    ).as_str()
                                );

                                let context = serde_json::to_string(&FileTransferNodeContext {
                                    node: message.wire.source_ship,
                                }).unwrap();
                                bindings::yield_results(vec![
                                    (
                                        bindings::WitProtomessage {
                                            protomessage_type: WitProtomessageType::Request(
                                                WitRequestTypeWithTarget {
                                                    is_expecting_response: true,
                                                    target_ship: &our_name,
                                                    target_app: "filesystem",
                                                },
                                            ),
                                            payload: &WitPayload {
                                                json: Some(serde_json::to_string(
                                                    &FileSystemRequest {
                                                        uri_string: uri_string.clone(),
                                                        action: FileSystemAction::OpenAppend,
                                                    }
                                                ).unwrap()),
                                                bytes: None,
                                            },
                                        },
                                        context.as_str(),
                                    ),
                                ].as_slice());
                                print_to_terminal("Started 7");
                            },
                            FileTransferResponse::PieceReceived(piece_received) => {
                                print_to_terminal("PieceReceived");

                                let key = FileTransferKey {
                                    requester: message.wire.source_ship.clone(),
                                    server: our_name.clone(),
                                    uri_string: piece_received.uri_string.clone(),
                                };

                                let uploading = uploads.get_mut(&key).unwrap();

                                if uploading.sent_pieces.len() - 1 < piece_received.piece_number as usize {
                                    panic!("PieceReceived: piece number too big");
                                }

                                uploading.sent_pieces[piece_received.piece_number as usize].is_confirmed = true;

                                let context = serde_json::to_string(&FileTransferNodeContext {
                                    node: message.wire.source_ship,
                                }).unwrap();

                                bindings::yield_results(vec![
                                    (
                                        bindings::WitProtomessage {
                                            protomessage_type: WitProtomessageType::Request(
                                                WitRequestTypeWithTarget {
                                                    is_expecting_response: true,
                                                    target_ship: &our_name,
                                                    target_app: "filesystem",
                                                },
                                            ),
                                            payload: &WitPayload {
                                                json: Some(serde_json::to_string(
                                                    &FileSystemRequest {
                                                        uri_string: piece_received.uri_string,
                                                        action: FileSystemAction::ReadChunkFromOpen(
                                                            uploading.metadata.chunk_size,
                                                        ),
                                                    }
                                                ).unwrap()),
                                                bytes: None,
                                            },
                                        },
                                        context.as_str(),
                                    )
                                ].as_slice());
                            },
                            FileTransferResponse::FileReceived(uri_string) => {
                                print_to_terminal("FileReceived");

                                let key = FileTransferKey {
                                    requester: message.wire.source_ship.clone(),
                                    server: our_name.clone(),
                                    uri_string,
                                };

                                uploads.remove(&key).unwrap();
                            },
                            FileTransferResponse::Errored(_error) => {
                                panic!("errored: todo");
                            },
                        }
                    }
                },
            }
        }
    }
}

bindings::export!(Component);
