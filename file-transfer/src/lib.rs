use std::collections::HashMap;
use serde::{Serialize, Deserialize};
use thiserror::Error;
use bindings::print_to_terminal;
use bindings::component::microkernel_process::types::WitMessageType;
use bindings::component::microkernel_process::types::WitPayload;
use bindings::component::microkernel_process::types::WitProtomessageType;
use bindings::component::microkernel_process::types::WitRequestTypeWithTarget;

struct Component;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessNode {
    pub node: String,
    pub process: String,
}

#[derive(Error, Debug, Serialize, Deserialize)]
pub enum NetworkingError {
    #[error("Peer is offline or otherwise unreachable")]
    PeerOffline,
    #[error("Message delivery failed due to timeout")]
    MessageTimeout,
    #[error("Some bug in the networking code")]
    NetworkingBug,
}

#[derive(Error, Debug, Serialize, Deserialize)]
pub enum FileSystemError {
    //  bad input from user
    #[error("Malformed URI: {uri}. Problem with {bad_part_name}: {:?}.", bad_part)]
    BadUri { uri: String, bad_part_name: String,  bad_part: Option<String>, },
    #[error("JSON payload could not be parsed to FileSystemRequest: {error}. Got {:?}.", json)]
    BadJson { json: Option<serde_json::Value>, error: String, },
    #[error("Bytes payload required for {action}.")]
    BadBytes { action: String },
    #[error("{process_name} not allowed to access {attempted_dir}. Process may only access within {sandbox_dir}.")]
    IllegalAccess { process_name: String, attempted_dir: String, sandbox_dir: String, },
    #[error("Already have {path} opened with mode {mode}.")]
    AlreadyOpen { path: String, mode: String, },
    #[error("Don't have {path} opened with mode {mode}.")]
    NotCurrentlyOpen { path: String, mode: String, },
    //  path or underlying fs problems
    #[error("Failed to join path: base: '{base_path}'; addend: '{addend}'.")]
    BadPathJoin { base_path: String, addend: String, },
    #[error("Failed to create dir at {path}: {error}.")]
    CouldNotMakeDir { path: String, error: String, },
    #[error("Failed to read {path}: {error}.")]
    ReadFailed { path: String, error: String, },
    #[error("Failed to write {path}: {error}.")]
    WriteFailed { path: String, error: String, },
    #[error("Failed to open {path} for {mode}: {error}.")]
    OpenFailed { path: String, mode: String, error: String, },
    #[error("Filesystem error while {what} on {path}: {error}.")]
    FsError { what: String, path: String, error: String, },
}
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
    Error(FileSystemError),
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
    target_ship: String,
    uri_string: String,
}
// #[derive(Debug, Serialize, Deserialize)]
// struct FileTransferReadyToReceive {
//     uri_string: String,
// }
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum FileTransferRequest {
    //  actions
    GetFile(FileTransferGetFile),    //  from user to requester
    Start(FileTransferStart),        //  from requester to server
    Cancel(FileTransferCancel),      //  from user to requester & requester to server
    GetPiece(FileTransferGetPiece),  //  from requester to server
    //  updates
    // ReadyToReceive(FileTransferReadyToReceive),  //  from 
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
    Started(FileTransferMetadata),     //  from server to requester
    FilePiece(FileTransferFilePiece),  //  from server to requester
    // FileReceived(String),              //  uri_string to derive key
    Errored(FileTransferError),
}

struct Downloading {
    metadata: FileTransferMetadata,
    received_pieces: Vec<u64>,  //  piece_hash
}
struct Uploading {
    metadata: FileTransferMetadata,
    number_sent_pieces: u32,
}
type Downloads = HashMap<FileTransferKey, Downloading>;
type Uploads = HashMap<FileTransferKey, Uploading>;

#[derive(Debug, Serialize, Deserialize)]
struct FileTransferContext {
    key: FileTransferKey,
    additional: FileTransferAdditionalContext,
}
#[derive(Debug, Serialize, Deserialize)]
enum FileTransferAdditionalContext {
    Empty,
    Metadata { chunk_size: u64 },
    Piece { piece_number: u32 },
}

fn div_round_up(numerator: u64, denominator: u64) -> u64 {
    (numerator + denominator - 1) / denominator
}

fn clean_up_state(
    our_name: String,
    process_name: String,
    context: String,
    downloads: &mut Downloads,
    uploads: &mut Uploads,
) {
    let context: FileTransferContext = serde_json::from_str(&context).unwrap();
    if our_name == context.key.requester {
        downloads.remove(&context.key);
    } else if our_name == context.key.server {
        uploads.remove(&context.key);
    } else {
        panic!("file_transfer: cleaning up state failed: we are neither requester nor server")
    }
}

fn handle_networking_error(
    error: NetworkingError,
    our_name: String,
    process_name: String,
    context: String,
    downloads: &mut Downloads,
    uploads: &mut Uploads,
) {
    match error {
        NetworkingError::PeerOffline => {
            panic!("")
        },
        NetworkingError::MessageTimeout => {
            panic!("")
        },
        NetworkingError::NetworkingBug => {
            panic!("")
        },
    }
}

fn handle_fs_error(
    error: FileSystemError,
    our_name: String,
    process_name: String,
    context: String,
    downloads: &mut Downloads,
    uploads: &mut Uploads,
) {
    match error {
        //  bad input from user
        FileSystemError::BadUri { uri, bad_part_name, bad_part, } => {
            panic!("")
        },
        FileSystemError::BadJson { json, error, } => {
            panic!("")
        },
        FileSystemError::BadBytes { action, } => {
            panic!("")
        },
        FileSystemError::IllegalAccess { process_name, attempted_dir, sandbox_dir, } => {
            panic!("")
        },
        FileSystemError::AlreadyOpen { path, mode, } => {
            if "Append" == mode {
                print_to_terminal("AlreadyOpen: Append");

                let context: FileTransferContext = serde_json::from_str(&context).unwrap();
                let downloading = downloads.get(&context.key).unwrap();

                yield_get_piece(
                    ProcessNode {
                        node: context.key.server.clone(),
                        process: process_name,
                    },
                    context.key.uri_string.clone(),
                    downloading.metadata.chunk_size,
                    downloading.received_pieces.len() as u32,
                );
            } else if "Read" == mode {
                print_to_terminal("AlreadyOpen: Read");
            } else {
                panic!("")
            }
        },
        FileSystemError::NotCurrentlyOpen { path, mode, } => {
            panic!("")
        },
        //  path or underlying fs problems
        FileSystemError::BadPathJoin { base_path, addend, } => {
            panic!("")
        },
        FileSystemError::CouldNotMakeDir { path, error, } => {
            panic!("")
        },
        FileSystemError::ReadFailed {path, error, } => {
            panic!("")
        },
        FileSystemError::WriteFailed { path, error, } => {
            panic!("")
        },
        FileSystemError::OpenFailed { path, mode, error, } => {
            panic!("")
        },
        FileSystemError::FsError { what, path, error, } => {
            panic!("")
        },
    }
}

fn yield_get_piece(
    target: ProcessNode,
    uri_string: String,
    chunk_size: u64,
    piece_number: u32,
) {
    bindings::yield_results(vec![
        (
            bindings::WitProtomessage {
                protomessage_type: WitProtomessageType::Request(
                    WitRequestTypeWithTarget {
                        is_expecting_response: true,
                        target_ship: &target.node,
                        target_app: &target.process,
                    }
                ),
                payload: &WitPayload {
                    json: Some(serde_json::to_string(
                        &FileTransferRequest::GetPiece(
                            FileTransferGetPiece {
                                uri_string,
                                chunk_size,
                                piece_number,
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



impl bindings::MicrokernelProcess for Component {
    fn run_process(our_name: String, process_name: String) {
        print_to_terminal("file_transfer: begin");

        //  in progress
        let mut downloads: Downloads = HashMap::new();
        let mut uploads: Uploads = HashMap::new();

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

                            //  TODO: if already transferring that file to someone else, bail

                            let context = serde_json::to_string(&FileTransferContext {
                                key: FileTransferKey {
                                    requester: message.wire.source_ship,
                                    server: our_name.clone(),
                                    uri_string: start.uri_string.clone(),
                                },
                                additional: FileTransferAdditionalContext::Metadata {
                                    chunk_size: start.chunk_size,
                                },
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
                        FileTransferRequest::GetPiece(get_piece) => {
                            print_to_terminal("GetPiece");

                            let key = FileTransferKey {
                                requester: message.wire.source_ship.clone(),
                                server: our_name.clone(),
                                uri_string: get_piece.uri_string.clone(),
                            };

                            let uploading = uploads.get(&key).unwrap();
                            let context = serde_json::to_string(&FileTransferContext {
                                key,
                                additional: FileTransferAdditionalContext::Piece {
                                    piece_number: get_piece.piece_number.clone(),
                                },
                            }).unwrap();
                            let payload =
                                if uploading.number_sent_pieces == get_piece.piece_number {
                                    WitPayload {
                                        json: Some(serde_json::to_string(
                                            &FileSystemRequest {
                                                uri_string: get_piece.uri_string,
                                                action: FileSystemAction::ReadChunkFromOpen(
                                                    get_piece.chunk_size,
                                                ),
                                            }
                                        ).unwrap()),
                                        bytes: None,
                                    }
                                } else {
                                    //  requester is resuming a previous download:
                                    //   make sure file handle is seeked to right place
                                    WitPayload {
                                        json: Some(serde_json::to_string(
                                            &FileSystemRequest {
                                                uri_string: get_piece.uri_string,
                                                action: FileSystemAction::SeekWithinOpen(
                                                    FileSystemSeekFrom::Start(
                                                        (get_piece.piece_number as u64) * get_piece.chunk_size
                                                    ),
                                                ),
                                            }
                                        ).unwrap()),
                                        bytes: None,
                                    }
                                };
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
                                        payload: &payload,
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

                                let context: FileTransferContext =
                                    serde_json::from_str(&context).unwrap();
                                let FileTransferAdditionalContext::Metadata { chunk_size }
                                        = context.additional else {
                                    panic!("file_transfer: GetMetadata Response requires chunk_size")
                                };
                                let key = context.key;
                                if file_metadata.uri_string != key.uri_string {
                                    panic!("file_transfer: GetMetadata Response non-matching uri_string")
                                }

                                let number_pieces = div_round_up(
                                    file_metadata.len,
                                    chunk_size
                                ) as u32;
                                let metadata = FileTransferMetadata {
                                    key: key.clone(),
                                    hash: file_metadata.hash,
                                    chunk_size,
                                    number_pieces,
                                    number_bytes: file_metadata.len,
                                };
                                uploads.insert(
                                    key.clone(),
                                    Uploading {
                                        metadata: metadata.clone(),
                                        number_sent_pieces: 0,
                                    }
                                );

                                let context = serde_json::to_string(&FileTransferContext {
                                    key,
                                    additional: FileTransferAdditionalContext::Empty,
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

                                let context: FileTransferContext = serde_json::from_str(&context).unwrap();
                                let key = context.key;
                                let downloading = downloads.get(&key).unwrap();
                                
                                yield_get_piece(
                                    ProcessNode {
                                        node: key.server.clone(),
                                        process: process_name.clone()
                                    },
                                    uri_string,
                                    downloading.metadata.chunk_size,
                                    downloading.received_pieces.len() as u32,
                                );
                                print_to_terminal("OpenAppend 2");
                            },
                            FileSystemResponse::ReadChunkFromOpen(uri_hash) => {
                                print_to_terminal("ReadChunkFromOpen");

                                let context: FileTransferContext = serde_json::from_str(&context).unwrap();
                                let FileTransferAdditionalContext::Piece { piece_number } = context.additional else {
                                    panic!("ReadChunkFromOpen: no piece_number in context");
                                };
                                let Some(bytes) = message.payload.bytes.clone() else {
                                    panic!("ReadChunkFromOpen: no bytes");
                                };
                                let key = context.key;
                                if key.uri_string != uri_hash.uri_string {
                                    panic!("file_transfer: ReadChunkFromOpen Response non-matching uri_string")
                                }

                                let uploading = uploads.get_mut(&key).unwrap();

                                if uploading.number_sent_pieces != piece_number {
                                    print_to_terminal(format!(
                                        "file_transfer: piece_number {} differs from state {}: assuming this is a resumed session",
                                        piece_number,
                                        uploading.number_sent_pieces,
                                    ).as_str());
                                }

                                uploading.number_sent_pieces = piece_number.clone() + 1;

                                bindings::yield_results(vec![
                                    (
                                        bindings::WitProtomessage {
                                            protomessage_type: WitProtomessageType::Response,
                                            payload: &WitPayload {
                                                json: Some(serde_json::to_string(
                                                    &FileTransferResponse::FilePiece(
                                                        FileTransferFilePiece {
                                                            uri_string: uri_hash.uri_string,
                                                            piece_number,
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
                                
                                let context: FileTransferContext =
                                    serde_json::from_str(&context).unwrap();
                                let FileTransferAdditionalContext::Piece{ piece_number } = 
                                        context.additional else {
                                    panic!("file_transfer: Append Response requires piece_number")
                                };

                                let key = context.key;

                                let downloading = downloads.remove(&key).unwrap();
                                if downloading.received_pieces.len() == downloading.metadata.number_pieces as usize {
                                    //  received all file pieces
                                    //  TODO: check hash is correct
                                    continue;
                                } else {
                                    //  still expecting file pieces
                                    let chunk_size = downloading.metadata.chunk_size.clone();
                                    let piece_number = downloading.received_pieces.len() as u32;
                                    downloads.insert(key.clone(), downloading);

                                    yield_get_piece(
                                        ProcessNode {
                                            node: key.server.clone(),
                                            process: process_name.clone()
                                        },
                                        uri_string,
                                        chunk_size,
                                        piece_number,
                                    );
                                }
                            },
                            FileSystemResponse::SeekWithinOpen(uri_string) => {
                                let parsed_context: FileTransferContext =
                                    serde_json::from_str(&context).unwrap();
                                let FileTransferAdditionalContext::Piece { piece_number } = 
                                        parsed_context.additional else {
                                    panic!("SeekWithinOpen needs piece_number context");
                                };
                                let uploading = uploads.get(&parsed_context.key).unwrap();

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
                                                        uri_string: uri_string,
                                                        action: FileSystemAction::ReadChunkFromOpen(
                                                            uploading.metadata.chunk_size.clone(),
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
                            FileSystemResponse::Error(error) => {
                                handle_fs_error(
                                    error,
                                    our_name.clone(),
                                    process_name.clone(),
                                    context,
                                    &mut downloads,
                                    &mut uploads
                                );
                                // panic!("file_transfer: got error: {}", error);
                            },
                            _ => {
                                panic!(
                                    "file_transfer: panic: unexpected filesystem Response: {:?}",
                                    response,
                                );
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
                                if !downloads.contains_key(&key) {
                                    downloads.insert(
                                        key.clone(),
                                        Downloading {
                                            metadata,
                                            received_pieces: vec![],
                                        }
                                    );
                                }
                                print_to_terminal("Started 6");
                                print_to_terminal(
                                    format!(
                                        "Started downloads keys: {:?}",
                                        downloads.keys().collect::<Vec<_>>(),
                                    ).as_str()
                                );

                                let context = serde_json::to_string(&FileTransferContext {
                                    key,
                                    additional: FileTransferAdditionalContext::Empty,
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
                            FileTransferResponse::FilePiece(file_piece) => {
                                print_to_terminal("FilePiece");

                                //  TODO: confirm bytes match alleged piece hash

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

                                let context = serde_json::to_string(&FileTransferContext {
                                    key,
                                    additional: FileTransferAdditionalContext::Piece {
                                        piece_number: file_piece.piece_number,
                                    },
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
                            // FileTransferResponse::FileReceived(uri_string) => {
                            //     print_to_terminal("FileReceived");

                            //     let key = FileTransferKey {
                            //         requester: message.wire.source_ship.clone(),
                            //         server: our_name.clone(),
                            //         uri_string,
                            //     };

                            //     uploads.remove(&key).unwrap();
                            // },
                            FileTransferResponse::Errored(_error) => {
                                panic!("errored: todo");
                            },
                        }
                    } else if "ws" == message.wire.source_app {
                        if let Ok(networking_error) =
                                serde_json::from_str::<NetworkingError>(payload_json_string) {
                            handle_networking_error(
                                networking_error,
                                our_name.clone(),
                                process_name.clone(),
                                context,
                                &mut downloads,
                                &mut uploads,
                            );
                        }
                    }
                },
            }
        }
    }
}

bindings::export!(Component);
