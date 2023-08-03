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
    #[error("Already have {path} opened with mode {:?}.", mode)]
    AlreadyOpen { path: String, mode: FileSystemMode, },
    #[error("Don't have {path} opened with mode {:?}.", mode)]
    NotCurrentlyOpen { path: String, mode: FileSystemMode, },
    //  path or underlying fs problems
    #[error("Failed to join path: base: '{base_path}'; addend: '{addend}'.")]
    BadPathJoin { base_path: String, addend: String, },
    #[error("Failed to create dir at {path}: {error}.")]
    CouldNotMakeDir { path: String, error: String, },
    #[error("Failed to read {path}: {error}.")]
    ReadFailed { path: String, error: String, },
    #[error("Failed to write {path}: {error}.")]
    WriteFailed { path: String, error: String, },
    #[error("Failed to open {path} for {:?}: {error}.", mode)]
    OpenFailed { path: String, mode: FileSystemMode, error: String, },
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
    ReadDir,
    Open(FileSystemMode),
    Close(FileSystemMode),
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
    ReadDir(Vec<FileSystemMetadata>),
    Open { uri_string: String, mode: FileSystemMode },
    Close { uri_string: String, mode: FileSystemMode },
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
    pub hash: Option<u64>,
    pub entry_type: FileSystemEntryType,
    pub len: u64,
}
#[derive(Eq, Hash, PartialEq, Clone, Debug, Serialize, Deserialize)]
pub enum FileSystemMode {
    Read,
    Append,
    AppendOverwrite,
}
#[derive(Debug, Serialize, Deserialize)]
pub enum FileSystemEntryType {
    Symlink,
    File,
    Dir,
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
// #[derive(Debug, Serialize, Deserialize)]
// struct FileTransferReadyToReceive {
//     uri_string: String,
// }
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum FileTransferRequest {
    GetFile(FileTransferGetFile),                         //  from user to requester
    Start(FileTransferStart),                             //  from requester to server
    Cancel { key: FileTransferKey, is_cancel_both: bool, reason: String },
    GetPiece(FileTransferGetPiece),                       //  from requester to server
    Done { uri_string: String },                          //  from requester to server
    DisplayOngoing,                                       //  from user to requester
    ReadDir { target_node: String, uri_string: String, }  //  from user to requester to server
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
enum FileTransferResponse {
    Started(FileTransferMetadata),     //  from server to requester
    FilePiece(FileTransferFilePiece),  //  from server to requester
    ReadDir(Vec<FileSystemMetadata>),  //  from server to requester
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

const FILE_TRANSFER_PAGE: &str = include_str!("file-transfer.html");

fn div_round_up(numerator: u64, denominator: u64) -> u64 {
    (numerator + denominator - 1) / denominator
}

fn bail(
    error: String,
    our_name: &str,
    process_name: &str,
    key: FileTransferKey,
) {
    let context = serde_json::to_string(&FileTransferContext {
        key: key.clone(),
        additional: FileTransferAdditionalContext::Empty,
    }).unwrap();
    yield_cancel(
        our_name,
        process_name,
        key,
        true,
        format!("{}: {}", process_name, error),
        context.as_str(),
    );
}

fn handle_networking_error(
    error: NetworkingError,
    our_name: &str,
    process_name: &str,
    key: FileTransferKey,
) {
    bail(format!("NetworkingError: {}", error), our_name, process_name, key);
    // match error {
    //     NetworkingError::PeerOffline => {
    //     },
    //     NetworkingError::MessageTimeout => {
    //     },
    //     NetworkingError::NetworkingBug => {
    //     },
    // }
}

fn handle_fs_error(
    error: FileSystemError,
    our_name: &str,
    process_name: &str,
    key: FileTransferKey,
    downloads: &mut Downloads,
    uploads: &mut Uploads,
) {
    match error {
        // //  bad input from user
        // FileSystemError::BadUri { uri, bad_part_name, bad_part, } => {
        // },
        // FileSystemError::BadJson { json, error, } => {
        // },
        // FileSystemError::BadBytes { action, } => {
        // },
        // FileSystemError::IllegalAccess { process_name, attempted_dir, sandbox_dir, } => {
        // },
        FileSystemError::AlreadyOpen { path, mode, } => {
            match mode {
                FileSystemMode::Append => {
                    print_to_terminal("AlreadyOpen: Append");

                    let downloading = downloads.get(&key).unwrap();

                    yield_get_piece(
                        ProcessNode {
                            node: key.server.clone(),
                            process: process_name.into(),
                        },
                        key.uri_string.clone(),
                        downloading.metadata.chunk_size,
                        downloading.received_pieces.len() as u32,
                    );
                },
                FileSystemMode::Read => print_to_terminal("AlreadyOpen: Read"),
                _ => {},
            }
        },
        // FileSystemError::NotCurrentlyOpen { path, mode, } => {
        // },
        // //  path or underlying fs problems
        // FileSystemError::BadPathJoin { base_path, addend, } => {
        // },
        // FileSystemError::CouldNotMakeDir { path, error, } => {
        // },
        // FileSystemError::ReadFailed {path, error, } => {
        // },
        // FileSystemError::WriteFailed { path, error, } => {
        // },
        // FileSystemError::OpenFailed { path, mode, error, } => {
        // },
        // FileSystemError::FsError { what, path, error, } => {
        // },
        _ => {
            bail(format!("FileSystemError: {}", error), our_name, process_name, key);
        }
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

fn yield_get_metadata(
    our_name: &str,
    uri_string: String,
    context: &str,
) {
    bindings::yield_results(vec![
        (
            bindings::WitProtomessage {
                protomessage_type: WitProtomessageType::Request(
                    WitRequestTypeWithTarget {
                        is_expecting_response: true,
                        target_ship: our_name,
                        target_app: "filesystem",
                    },
                ),
                payload: &WitPayload {
                    json: Some(serde_json::to_string(
                        &FileSystemRequest {
                            uri_string: uri_string,
                            action: FileSystemAction::GetMetadata,
                        }
                    ).unwrap()),
                    bytes: None,
                },
            },
            context,
        ),
    ].as_slice());
}

fn yield_get_file(
    our_name: &str,
    process_name: &str,
    target_node: String,
    uri_string: String,
    chunk_size: u64,
) {
    bindings::yield_results(vec![
        (
            bindings::WitProtomessage {
                protomessage_type: WitProtomessageType::Request(
                    WitRequestTypeWithTarget {
                        is_expecting_response: false,
                        target_ship: our_name,
                        target_app: process_name,
                    },
                ),
                payload: &WitPayload {
                    json: Some(serde_json::to_string(
                        &FileTransferRequest::GetFile(
                            FileTransferGetFile {
                                target_ship: target_node,
                                uri_string,
                                chunk_size,
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

fn yield_start(
    target: ProcessNode,
    uri_string: String,
    chunk_size: u64,
) {
    bindings::yield_results(vec![
        (
            bindings::WitProtomessage {
                protomessage_type: WitProtomessageType::Request(
                    WitRequestTypeWithTarget {
                        is_expecting_response: true,
                        target_ship: &target.node,
                        target_app: &target.process,
                    },
                ),
                payload: &WitPayload {
                    json: Some(serde_json::to_string(
                        &FileTransferRequest::Start(
                            FileTransferStart {
                                uri_string,
                                chunk_size,
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

fn yield_close(
    our_name: &str,
    uri_string: String,
    mode: FileSystemMode,
    context: &str,
) {
    bindings::yield_results(vec![
        (
            bindings::WitProtomessage {
                protomessage_type: WitProtomessageType::Request(
                    WitRequestTypeWithTarget {
                        is_expecting_response: true,
                        target_ship: our_name,
                        target_app: "filesystem",
                    },
                ),
                payload: &WitPayload {
                    json: Some(serde_json::to_string(
                        &FileSystemRequest {
                            uri_string,
                            action: FileSystemAction::Close(mode),
                        }
                    ).unwrap()),
                    bytes: None,
                },
            },
            context,
        ),
    ].as_slice());
}

fn yield_cancel(
    target_node: &str,
    process_name: &str,
    key: FileTransferKey,
    is_cancel_both: bool,
    reason: String,
    context: &str,
) {
    bindings::yield_results(vec![
        (
            bindings::WitProtomessage {
                protomessage_type: WitProtomessageType::Request(
                    WitRequestTypeWithTarget {
                        is_expecting_response: false,
                        target_ship: &target_node,
                        target_app: &process_name,
                    },
                ),
                payload: &WitPayload {
                    json: Some(serde_json::to_string(
                        &FileTransferRequest::Cancel {
                            key,
                            is_cancel_both,
                            reason,
                        }
                    ).unwrap()),
                    bytes: None,
                },
            },
            context,
        )
    ].as_slice());
}

impl bindings::MicrokernelProcess for Component {
    fn run_process(our_name: String, process_name: String) {
        print_to_terminal("file_transfer: begin");
        // HTTP bindings
        bindings::yield_results(
            vec![(
                bindings::WitProtomessage {
                    protomessage_type: WitProtomessageType::Request(
                        WitRequestTypeWithTarget {
                            is_expecting_response: false,
                            target_ship: our_name.as_str(),
                            target_app: "http_bindings",
                        }
                    ),
                    payload: &WitPayload {
                        json: Some(serde_json::json!({
                            "action": "bind-app",
                            "path": "/apps/file-transfer",
                            "app": process_name
                        }).to_string()),
                        bytes: None
                    }
                },
                "",
            ), (
                bindings::WitProtomessage {
                    protomessage_type: WitProtomessageType::Request(
                        WitRequestTypeWithTarget {
                            is_expecting_response: false,
                            target_ship: our_name.as_str(),
                            target_app: "http_bindings",
                        }
                    ),
                    payload: &WitPayload {
                        json: Some(serde_json::json!({
                            "action": "bind-app",
                            "path": "/file-transfer/view-files/:username",
                            "app": process_name
                        }).to_string()),
                        bytes: None
                    }
                },
                "",
            ), (
                bindings::WitProtomessage {
                    protomessage_type: WitProtomessageType::Request(
                        WitRequestTypeWithTarget {
                            is_expecting_response: false,
                            target_ship: our_name.as_str(),
                            target_app: "http_bindings",
                        }
                    ),
                    payload: &WitPayload {
                        json: Some(serde_json::json!({
                            "action": "bind-app",
                            "path": "/file-transfer/get-file",
                            "app": process_name
                        }).to_string()),
                        bytes: None
                    }
                },
                "",
            ), (
                bindings::WitProtomessage {
                    protomessage_type: WitProtomessageType::Request(
                        WitRequestTypeWithTarget {
                            is_expecting_response: false,
                            target_ship: our_name.as_str(),
                            target_app: "http_bindings",
                        }
                    ),
                    payload: &WitPayload {
                        json: Some(serde_json::json!({
                            "action": "bind-app",
                            "path": "/file-transfer/get-file/cancel",
                            "app": process_name
                        }).to_string()),
                        bytes: None
                    }
                },
                "",
            )].as_slice()
        );

        //  in progress
        let mut downloads: Downloads = HashMap::new();
        let mut uploads: Uploads = HashMap::new();

        'main_loop: loop {
            let (message, context) = bindings::await_next_message();
            let Some(ref payload_json_string) = message.payload.json else {
                print_to_terminal("file_transfer: require non-empty json payload");
                continue;
            };

            print_to_terminal(
                format!("{}: got json {}", process_name, payload_json_string).as_str()
            );

            let message_from_loop: serde_json::Value = serde_json::from_str(&payload_json_string).unwrap();
            if message_from_loop["method"] == "GET" && message_from_loop["path"] == "/apps/file-transfer" {
                bindings::yield_results(vec![(
                    bindings::WitProtomessage {
                        protomessage_type: WitProtomessageType::Response,
                        payload: &WitPayload {
                            json: Some(serde_json::json!({
                                "action": "response",
                                "status": 200,
                                "headers": {
                                    "Content-Type": "text/html",
                                },
                            }).to_string()),
                            bytes: Some(FILE_TRANSFER_PAGE.replace("${our}", &our_name).as_bytes().to_vec())
                        }
                    },
                    "",
                )].as_slice());
            } else if message_from_loop["method"] == "GET" && message_from_loop["path"] == "/file-transfer/view-files/:username" {
                let target_node = message_from_loop["url_params"]["username"].as_str().unwrap_or("");
                let uri_string = String::from("fs://.");

                if target_node.is_empty() {
                    print_to_terminal("file_transfer: target_node is empty");
                    bindings::yield_results(vec![(
                        bindings::WitProtomessage {
                            protomessage_type: WitProtomessageType::Response,
                            payload: &WitPayload {
                                json: Some(serde_json::json!({
                                    "action": "response",
                                    "status": 400,
                                    "headers": {
                                        "Content-Type": "text/html",
                                    },
                                }).to_string()),
                                bytes: Some("Must specify target node".as_bytes().to_vec())
                            }
                        },
                        "",
                    )].as_slice());
                    continue;
                }

                let context = serde_json::to_string(&FileTransferContext {
                    key: FileTransferKey {
                        requester: our_name.clone(),
                        server: target_node.to_string(),
                        uri_string: uri_string.clone(),
                    },
                    additional: FileTransferAdditionalContext::Empty,
                }).unwrap();

                let (message, _) = if our_name == target_node {
                    bindings::yield_and_await_response((
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
                                        uri_string,
                                        action: FileSystemAction::ReadDir,
                                    }
                                ).unwrap()),
                                bytes: None,
                            },
                        },
                        context.as_str(),
                    ))
                } else {
                    bindings::yield_and_await_response((
                        bindings::WitProtomessage {
                            protomessage_type: WitProtomessageType::Request(
                                WitRequestTypeWithTarget {
                                    is_expecting_response: true,
                                    target_ship: &target_node,
                                    target_app: &process_name,
                                },
                            ),
                            payload: &WitPayload {
                                json: Some(serde_json::to_string(
                                    &FileTransferRequest::ReadDir {
                                        target_node: target_node.to_string(),
                                        uri_string: uri_string.clone(),
                                    }
                                ).unwrap()),
                                bytes: None,
                            },
                        },
                        context.as_str(),
                    ))
                };

                let Some(ref payload_json_string) = message.payload.json else {
                    print_to_terminal("file_transfer: require non-empty json payload");
                    bindings::yield_results(vec![(
                        bindings::WitProtomessage {
                            protomessage_type: WitProtomessageType::Response,
                            payload: &WitPayload {
                                json: Some(serde_json::json!({
                                    "action": "response",
                                    "status": 404,
                                    "headers": {
                                        "Content-Type": "text/html",
                                    },
                                }).to_string()),
                                bytes: Some("No result from target node".as_bytes().to_vec())
                            }
                        },
                        "",
                    )].as_slice());
                    continue;
                };

                bindings::yield_results(vec![(
                    bindings::WitProtomessage {
                        protomessage_type: WitProtomessageType::Response,
                        payload: &WitPayload {
                            json: Some(serde_json::json!({
                                "action": "response",
                                "status": 200,
                                "headers": {
                                    "Content-Type": "text/html",
                                },
                            }).to_string()),
                            // {"ReadDir":[{"entry_type":"File","hash":null,"len":7219,"uri_string":"README.md"}]}
                            bytes: Some(payload_json_string.as_bytes().to_vec())
                        }
                    },
                    "",
                )].as_slice());
            } else if message_from_loop["method"] == "POST" && message_from_loop["path"] == "/file-transfer/get-file" {
                // {"ReadDir":[{"entry_type":"File","hash":null,"len":7219,"uri_string":"README.md"}]}
                let body_bytes = message.payload.bytes.unwrap_or(vec![]);
                let body_json_string = match String::from_utf8(body_bytes) {
                    Ok(s) => s,
                    Err(_) => String::new()
                };
                print_to_terminal(format!("BODY: {}", body_json_string).as_str());
                let body: serde_json::Value = serde_json::from_str(&body_json_string).unwrap();

                yield_get_file(
                    &our_name,
                    &process_name,
                    body["target_node"].as_str().unwrap_or("").to_string(),
                    format!("fs://{}", body["uri_string"].as_str().unwrap_or("")),
                    body["chunk_size"].as_u64().unwrap_or(1024),
                );

                bindings::yield_results(vec![(
                    bindings::WitProtomessage {
                        protomessage_type: WitProtomessageType::Response,
                        payload: &WitPayload {
                            json: Some(serde_json::json!({
                                "action": "response",
                                "status": 204,
                                "headers": {
                                    "Content-Type": "text/html",
                                },
                            }).to_string()),
                            bytes: Some("Success".as_bytes().to_vec())
                        }
                    },
                    "",
                )].as_slice());
            } else {
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
                                //  if have server:file state:
                                //    get file metadata
                                //    if it exists and is partial file of proper size:
                                //      resume
                                //  else:
                                //    1. wipe state
                                //    2. close Append file handle, if it exists
                                //    3. open AppendOverwrite file handle
                                //    4. download from scratch
    
                                print_to_terminal("GetFile");
    
                                let key = FileTransferKey {
                                    requester: our_name.clone(),
                                    server: get_file.target_ship.clone(),
                                    uri_string: get_file.uri_string.clone(),
                                };
                                let context = serde_json::to_string(&FileTransferContext {
                                    key: key.clone(),
                                    additional: FileTransferAdditionalContext::Metadata {
                                        chunk_size: get_file.chunk_size,
                                    },
                                }).unwrap();
    
                                if downloads.contains_key(&key) {
                                    yield_get_metadata(
                                        &our_name,
                                        get_file.uri_string,
                                        &context,
                                    )
                                } else {
                                    yield_close(
                                        &our_name,
                                        get_file.uri_string,
                                        FileSystemMode::Append,
                                        context.as_str(),
                                    );
                                }
                            },
                            FileTransferRequest::Start(start) => {
                                print_to_terminal("Start");
    
                                let key =  FileTransferKey {
                                    requester: message.wire.source_ship,
                                    server: our_name.clone(),
                                    uri_string: start.uri_string.clone(),
                                };
    
                                //  if already transferring requested file to someone else, bail
                                for (other_key, _) in &uploads {
                                    if start.uri_string == other_key.uri_string {
                                        bail(
                                            format!(
                                                "transferring file {} to another user, please try again later",
                                                start.uri_string,
                                            ),
                                            &our_name,
                                            &process_name,
                                            key,
                                        );
                                        continue 'main_loop;
                                    }
                                }
    
                                let context = serde_json::to_string(&FileTransferContext {
                                    key,
                                    additional: FileTransferAdditionalContext::Metadata {
                                        chunk_size: start.chunk_size,
                                    },
                                }).unwrap();
    
                                yield_get_metadata(
                                    &our_name,
                                    start.uri_string,
                                    &context,
                                )
                            },
                            FileTransferRequest::Cancel { key, is_cancel_both, reason } => {
                                print_to_terminal("Cancel");
                                //  TODO: reason can leak information about server's machine
                                //        (e.g., full path of file that DNE);
                                //        figure out how to avoid that
                                print_to_terminal(format!(
                                    "file_transfer: Cancel received for {:?} with reason: {}",
                                    key,
                                    reason,
                                ).as_str());
    
                                let mode = 
                                    if key.server == our_name {
                                        uploads.remove(&key);
                                        FileSystemMode::Read
                                    } else if key.requester == our_name {
                                        downloads.remove(&key);
                                        FileSystemMode::Append
                                    } else {
                                        print_to_terminal("file_transfer: Cancel: must be either requester or server");
                                        continue;
                                    };
    
                                let context = serde_json::to_string(&FileTransferContext {
                                    key: key.clone(),
                                    additional: FileTransferAdditionalContext::Empty,
                                }).unwrap();
                                yield_close(
                                    &our_name,
                                    key.uri_string.clone(),
                                    mode,
                                    // context.as_str(),
                                    "",
                                );
    
                                if is_cancel_both {
                                    //  propagate cancel to other node
                                    let other_node =
                                        if key.server == our_name {
                                            key.requester.clone()
                                        } else if key.requester == our_name {
                                            key.server.clone()
                                        } else {
                                            print_to_terminal("file_transfer: Cancel: must be either requester or server");
                                            continue;
                                        };
                                    yield_cancel(
                                        &other_node,
                                        &process_name,
                                        key,
                                        false,
                                        reason,
                                        context.as_str(),
                                    );
                                }
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
                                    if (0 != uploading.number_sent_pieces) & (uploading.number_sent_pieces == get_piece.piece_number) {
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
                            FileTransferRequest::Done { uri_string } => {
                                let key = FileTransferKey {
                                    requester: message.wire.source_ship.clone(),
                                    server: our_name.clone(),
                                    uri_string: uri_string.clone(),
                                };
                                uploads.remove(&key);
                                yield_close(
                                    &our_name,
                                    uri_string.clone(),
                                    FileSystemMode::Read,
                                    ""
                                );
                                print_to_terminal(format!(
                                    "file_transfer: done transferring {} to {}",
                                    uri_string,
                                    message.wire.source_ship,
                                ).as_str());
                            },
                            FileTransferRequest::DisplayOngoing => {
                                print_to_terminal("file_transfer: ongoing downloads:");
                                print_to_terminal("****");
                                for (key, val) in downloads.iter() {
                                    print_to_terminal(format!(
                                        "remote://{}/{}",
                                        key.server,
                                        key.uri_string,
                                    ).as_str());
                                    print_to_terminal(format!(
                                        "  hash: {:?}",
                                        val.metadata.hash,
                                    ).as_str());
                                    print_to_terminal(format!(
                                        "  number_bytes: {}",
                                        val.metadata.number_bytes,
                                    ).as_str());
                                    print_to_terminal(format!(
                                        "  chunk size: {}",
                                        val.metadata.chunk_size,
                                    ).as_str());
                                    print_to_terminal(format!(
                                        "  chunks received / total: {} / {}",
                                        val.received_pieces.len(),
                                        val.metadata.number_pieces,
                                    ).as_str());
                                }
                                print_to_terminal("****");
                            },
                            FileTransferRequest::ReadDir { target_node, uri_string } => {
                                if our_name == target_node {
                                    //  serve Request:
                                    //    1. query local fs                   <---
                                    //    2. pass fs Reponse on to Requester
                                    let context = serde_json::to_string(&FileTransferContext {
                                        key: FileTransferKey {
                                            requester: message.wire.source_ship,
                                            server: our_name.clone(),
                                            uri_string: uri_string.clone(),
                                        },
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
                                                            uri_string,
                                                            action: FileSystemAction::ReadDir,
                                                        }
                                                    ).unwrap()),
                                                    bytes: None,
                                                },
                                            },
                                            context.as_str(),
                                        )
                                    ].as_slice());
                                } else {
                                    //  send Request to target
                                    // TODO: attach additional context to say it's http
                                    let context = serde_json::to_string(&FileTransferContext {
                                        key: FileTransferKey {
                                            requester: our_name.clone(),
                                            server: target_node.clone(),
                                            uri_string: uri_string.clone(),
                                        },
                                        additional: FileTransferAdditionalContext::Empty,
                                    }).unwrap();
                                    bindings::yield_results(vec![
                                        (
                                            bindings::WitProtomessage {
                                                protomessage_type: WitProtomessageType::Request(
                                                    WitRequestTypeWithTarget {
                                                        is_expecting_response: true,
                                                        target_ship: &target_node,
                                                        target_app: &process_name,
                                                    },
                                                ),
                                                payload: &WitPayload {
                                                    json: Some(serde_json::to_string(
                                                        &FileTransferRequest::ReadDir {
                                                            target_node: target_node.clone(),
                                                            uri_string: uri_string.clone(),
                                                        }
                                                    ).unwrap()),
                                                    bytes: None,
                                                },
                                            },
                                            context.as_str(),
                                        )
                                    ].as_slice());
                                }
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
                                        bail(
                                            "GetMetadata Response requires chunk_size".into(),
                                            &our_name,
                                            &process_name,
                                            context.key
                                        );
                                        continue;
                                    };
                                    if file_metadata.uri_string != context.key.uri_string {
                                        bail(
                                            "GetMetadata Response non-matching uri_string".into(),
                                            &our_name,
                                            &process_name,
                                            context.key
                                        );
                                        continue;
                                    }
    
                                    if our_name == context.key.server {
                                        //  server getting metadata of file-to-be-served
                                        let number_pieces = div_round_up(
                                            file_metadata.len,
                                            chunk_size
                                        ) as u32;
                                        let Some(hash) = file_metadata.hash else {
                                            bail(
                                                "GetMetadata did not get hash from fs".into(),
                                                &our_name,
                                                &process_name,
                                                context.key,
                                            );
                                            continue;
                                        };
                                        let metadata = FileTransferMetadata {
                                            key: context.key.clone(),
                                            hash,
                                            chunk_size,
                                            number_pieces,
                                            number_bytes: file_metadata.len,
                                        };
                                        uploads.insert(
                                            context.key.clone(),
                                            Uploading {
                                                metadata: metadata.clone(),
                                                number_sent_pieces: 0,
                                            }
                                        );
    
                                        let context = serde_json::to_string(&FileTransferContext {
                                            key: context.key,
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
                                                                action: FileSystemAction::Open(
                                                                    FileSystemMode::Read
                                                                ),
                                                            }
                                                        ).unwrap()),
                                                        bytes: None,
                                                    },
                                                },
                                                context.as_str(),
                                            ),
                                        ].as_slice());
                                    } else if our_name == context.key.requester {
                                        let Some(downloading) = downloads.get(&context.key) else {
                                            //  re-issue GetFile to self to download from scratch
                                            yield_get_file(
                                                &our_name,
                                                &process_name,
                                                context.key.server,
                                                context.key.uri_string,
                                                chunk_size,
                                            );
                                            continue;
                                        };
                                        if downloading.metadata.number_pieces == (downloading.received_pieces.len() as u32) {
                                            //  received all file pieces: check hash is correct
                                            if Some(downloading.metadata.hash) == file_metadata.hash {
                                                //  done! file successfully downloaded
                                                let context_string = serde_json::to_string(&FileTransferContext {
                                                    key: context.key.clone(),
                                                    additional: FileTransferAdditionalContext::Empty,
                                                }).unwrap();
                                                yield_close(
                                                    &our_name,
                                                    file_metadata.uri_string,
                                                    FileSystemMode::Append,
                                                    context_string.as_str(),
                                                );
                                                bindings::yield_results(vec![
                                                    (
                                                        bindings::WitProtomessage {
                                                            protomessage_type: WitProtomessageType::Request(
                                                                WitRequestTypeWithTarget {
                                                                    is_expecting_response: false,
                                                                    target_ship: &context.key.server,
                                                                    target_app: &process_name,
                                                                },
                                                            ),
                                                            payload: &WitPayload {
                                                                json: Some(serde_json::to_string(
                                                                    &FileTransferRequest::Done {
                                                                        uri_string: context.key.uri_string,
                                                                    }
                                                                ).unwrap()),
                                                                bytes: None,
                                                            },
                                                        },
                                                        "",
                                                    ),
                                                ].as_slice());
                                            } else {
                                                downloads.remove(&context.key);
                                                print_to_terminal("file_transfer: file corrupted during transfer, please try again");
                                            }
                                            continue;
                                        }
    
                                        //  requester getting metadata of possibly-resumable file
                                        if (chunk_size == downloading.metadata.chunk_size) & (file_metadata.len == chunk_size * (downloading.received_pieces.len() as u64)) {
                                            //  resume file transfer
                                            yield_start(
                                                ProcessNode {
                                                    node: context.key.server,
                                                    process: process_name.clone(),
                                                },
                                                context.key.uri_string,
                                                chunk_size,
                                            );
                                        } else {
                                            //  re-issue GetFile to self to download from scratch
                                            downloads.remove(&context.key);
                                            yield_get_file(
                                                &our_name,
                                                &process_name,
                                                context.key.server,
                                                context.key.uri_string,
                                                chunk_size,
                                            );
                                        }
                                    }
                                },
                                FileSystemResponse::ReadDir(metadatas) => {
                                    //  serve Request:
                                    //    1. query local fs
                                    //    2. pass fs Reponse on to Requester  <---
                                    bindings::yield_results(vec![
                                        (
                                            bindings::WitProtomessage {
                                                protomessage_type: WitProtomessageType::Response,
                                                payload: &WitPayload {
                                                    json: Some(serde_json::to_string(
                                                        &FileTransferResponse::ReadDir(metadatas)
                                                    ).unwrap()),
                                                    bytes: None,
                                                },
                                            },
                                            "",
                                        )
                                    ].as_slice());
                                },
                                FileSystemResponse::Open { uri_string, mode } => {
                                    match mode {
                                        FileSystemMode::Read => {
                                            print_to_terminal("Successfully opened Read")
                                        },
                                        FileSystemMode::Append => {
                                            print_to_terminal("OpenAppend");
    
                                            let context: FileTransferContext =
                                                serde_json::from_str(&context).unwrap();
                                            let downloading = downloads.get(&context.key).unwrap();
                                            yield_get_piece(
                                                ProcessNode {
                                                    node: context.key.server,
                                                    process: process_name.clone()
                                                },
                                                uri_string,
                                                downloading.metadata.chunk_size,
                                                downloading.received_pieces.len() as u32,
                                            )
                                        },
                                        FileSystemMode::AppendOverwrite => {
                                            //  AppendOverwrite case: fresh Start
                                            print_to_terminal("OpenAppendOverwrite");
    
                                            let context: FileTransferContext =
                                                serde_json::from_str(&context).unwrap();
                                            let FileTransferAdditionalContext::Metadata {
                                                chunk_size
                                            } = context.additional else {
                                                bail(
                                                    "Open AppendOverwrite requires chunk_size".into(),
                                                    &our_name,
                                                    &process_name,
                                                    context.key
                                                );
                                                continue;
                                            };
                                            yield_start(
                                                ProcessNode {
                                                    node: context.key.server,
                                                    process: process_name.clone(),
                                                },
                                                context.key.uri_string,
                                                chunk_size,
                                            );
                                        },
                                    }
                                },
                                FileSystemResponse::Close { uri_string, mode } => {
                                    match mode {
                                        FileSystemMode::Read => {
                                            print_to_terminal("Successfully closed Read")
                                        },
                                        FileSystemMode::Append => {
                                            print_to_terminal("CloseAppend");
    
                                            let parsed_context: FileTransferContext = match serde_json::from_str(&context) {
                                                Ok(pc) => pc,
                                                Err(e) => {
                                                    print_to_terminal("file_transfer: CloseAppend missing context to clean up");
                                                    continue;
                                                },
                                            };
    
                                            match downloads.remove(&parsed_context.key) {
                                                Some(_) => {
                                                    //  done downloading a file successfully
                                                    print_to_terminal(format!(
                                                        "file_transfer: successfully downloaded {} from {}",
                                                        parsed_context.key.uri_string,
                                                        parsed_context.key.server,
                                                    ).as_str());
                                                },
                                                None => {
                                                    //  starting a fresh download
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
                                                                            uri_string,
                                                                            action: FileSystemAction::Open(
                                                                                FileSystemMode::AppendOverwrite
                                                                            ),
                                                                        }
                                                                    ).unwrap()),
                                                                    bytes: None,
                                                                },
                                                            },
                                                            context.as_str(),
                                                        ),
                                                    ].as_slice());
                                                }
                                            }
                                        },
                                        _ => {},
                                    }
                                },
                                FileSystemResponse::ReadChunkFromOpen(uri_hash) => {
                                    print_to_terminal("ReadChunkFromOpen");
    
                                    let context: FileTransferContext = serde_json::from_str(&context).unwrap();
                                    let FileTransferAdditionalContext::Piece { piece_number } = context.additional else {
                                        bail(
                                            "ReadChunkFromOpen: no piece_number in context".into(),
                                            &our_name,
                                            &process_name,
                                            context.key
                                        );
                                        continue;
                                    };
                                    let Some(bytes) = message.payload.bytes.clone() else {
                                        bail(
                                            "ReadChunkFromOpen: no bytes".into(),
                                            &our_name,
                                            &process_name,
                                            context.key
                                        );
                                        continue;
                                    };
                                    if context.key.uri_string != uri_hash.uri_string {
                                        bail(
                                            "ReadChunkFromOpen Response non-matching uri_string".into(),
                                            &our_name,
                                            &process_name,
                                            context.key
                                        );
                                        continue;
                                    }
    
                                    let uploading = uploads.get_mut(&context.key).unwrap();
    
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
                                        bail(
                                            "Append Response requires piece_number".into(),
                                            &our_name,
                                            &process_name,
                                            context.key
                                        );
                                        continue;
                                    };
    
    
                                    let downloading = downloads.get(&context.key).unwrap();
                                    if downloading.received_pieces.len() == downloading.metadata.number_pieces as usize {
                                        //  received all file pieces: check hash is correct
                                        let context = serde_json::to_string(&FileTransferContext {
                                            key: context.key,
                                            additional: FileTransferAdditionalContext::Metadata {
                                                chunk_size: downloading.metadata.chunk_size,
                                            },
                                        }).unwrap();
                                        yield_get_metadata(
                                            &our_name,
                                            uri_string,
                                            &context,
                                        )
                                    } else {
                                        //  still expecting file pieces
                                        let chunk_size = downloading.metadata.chunk_size.clone();
                                        let piece_number = downloading.received_pieces.len() as u32;
    
                                        yield_get_piece(
                                            ProcessNode {
                                                node: context.key.server.clone(),
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
                                        bail(
                                            "SeekWithinOpen needs piece_number context".into(),
                                            &our_name,
                                            &process_name,
                                            parsed_context.key
                                        );
                                        continue;
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
                                    let context: FileTransferContext =
                                            match serde_json::from_str(&context) {
                                        Ok(c) => c,
                                        Err(_) => {
                                            print_to_terminal(format!(
                                                "file_transfer: FileSystemError: {:?}",
                                                error,
                                            ).as_str());
                                            continue;
                                        },
                                    };
    
                                    handle_fs_error(
                                        error,
                                        &our_name,
                                        &process_name,
                                        context.key,
                                        &mut downloads,
                                        &mut uploads
                                    );
                                },
                                _ => {
                                    print_to_terminal(format!(
                                        "file_transfer: panic: unexpected filesystem Response: {:?}",
                                        response,
                                    ).as_str());
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
                                        print_to_terminal("file_transfer: Started got incorrect key");
                                        continue;
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
                                                            action: FileSystemAction::Open(
                                                                FileSystemMode::Append
                                                            ),
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
                                    let key = FileTransferKey {
                                        requester: our_name.clone(),
                                        server: message.wire.source_ship.clone(),
                                        uri_string: file_piece.uri_string.clone(),
                                    };
    
                                    let Some(bytes) = message.payload.bytes.clone() else {
                                        bail(
                                            "FilePiece must be sent bytes".into(),
                                            &our_name,
                                            &process_name,
                                            key
                                        );
                                        continue;
                                    };
    
                                    let downloading = downloads.get_mut(&key).unwrap();
                                    if downloading.received_pieces.len() != file_piece.piece_number as usize {
                                        bail(
                                            "got out-of-order file piece; please retry download".into(),
                                            &our_name,
                                            &process_name,
                                            key
                                        );
                                        continue;
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
                                FileTransferResponse::ReadDir(metadatas) => {
                                    // TODO: read the context to see if this is HTTP
                                    let context: serde_json::Value =
                                        serde_json::from_str(&context).unwrap();
                                    print_to_terminal(format!(
                                        "file_transfer: directory contents of remote://{}/{}/",
                                        message.wire.source_ship,
                                        context["uri_string"].to_string(),
                                    ).as_str());
                                    print_to_terminal("****");
                                    for metadata in &metadatas {
                                        let suffix = match metadata.entry_type {
                                            FileSystemEntryType::Symlink => "@",
                                            FileSystemEntryType::File => "",
                                            FileSystemEntryType::Dir => "/",
                                        };
                                        print_to_terminal(format!(
                                            "{}{}\t{}",
                                            metadata.uri_string,
                                            suffix,
                                            metadata.len,
                                        ).as_str());
                                    }
                                    print_to_terminal("****");
                                },
                            }
                        } else if "ws" == message.wire.source_app {
                            if let Ok(networking_error) =
                                    serde_json::from_str::<NetworkingError>(payload_json_string) {
    
                                let context: FileTransferContext =
                                    serde_json::from_str(&context).unwrap();
    
                                handle_networking_error(
                                    networking_error,
                                    &our_name,
                                    &process_name,
                                    context.key,
                                );
                            }
                        }
                    },
                }
            }

        }
    }
}

bindings::export!(Component);
