use std::collections::HashMap;
use serde::{Serialize, Deserialize};
use thiserror::Error;

mod process_lib;

use bindings::print_to_terminal;
use bindings::component::microkernel_process::types::WitMessage;
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
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum FileTransferRequest {
    GetFile(FileTransferGetFile),                         //  from user to requester
    Start(FileTransferStart),                             //  from requester to server
    // Cancel { key: FileTransferKey, is_cancel_both: bool, reason: String },
    GetPiece(FileTransferGetPiece),                       //  from requester to server
    Done { uri_string: String },                          //  from requester to server
    // DisplayOngoing,                                       //  from user to requester
    // ReadDir { target_node: String, uri_string: String, }  //  from user to requester to server
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
    // ReadDir(Vec<FileSystemMetadata>),  //  from server to requester
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

enum MessageHandledStatus {
    ReadyForNext,
    Done,
}

fn div_round_up(numerator: u64, denominator: u64) -> u64 {
    (numerator + denominator - 1) / denominator
}

fn bail(
    error: String,
    our_name: &str,
    process_name: &str,
    key: FileTransferKey,
) {
    panic!("file_transfer: error on {:?}: {}", key, error);
}

fn handle_networking_error(
    error: NetworkingError,
    our_name: &str,
    process_name: &str,
    key: FileTransferKey,
) {
    bail(format!("NetworkingError: {}", error), our_name, process_name, key);
}

fn handle_fs_error(
    error: FileSystemError,
    our_name: &str,
    process_name: &str,
    key: FileTransferKey,
) {
    match error {
        _ => {
            bail(format!("FileSystemError: {}", error), our_name, process_name, key);
        }
    }
}

fn handle_message(
    message: WitMessage,
    context: String,
    mut uploading: &mut Option<Uploading>,
    our_name: &str,
    process_name: &str,
) -> anyhow::Result<MessageHandledStatus> {

    let our_filesystem = bindings::WitProcessNode {
        node: &our_name,
        process: "filesystem",
    };

    match message.message_type {
        WitMessageType::Request(_is_expecting_response) => {
            //  TODO: perms;
            //   only GetFile and Cancel allowed from non file_transfer
            //   and Cancel should probably only be allowed from same
            //   process as GetFile came from
            print_to_terminal("Request");
            match process_lib::parse_message_json(message.payload.json)? {
                FileTransferRequest::GetFile(get_file) => {
                    //  1. close Append file handle, if it exists
                    //  2. open AppendOverwrite file handle
                    //  3. download from scratch

                    print_to_terminal("GetFile");

                    let key = FileTransferKey {
                        requester: our_name.into(),
                        server: get_file.target_ship.clone(),
                        uri_string: get_file.uri_string.clone(),
                    };

                    let file_transfer_request_type = WitProtomessageType::Request(
                            WitRequestTypeWithTarget {
                                is_expecting_response: true,
                                target_ship: &get_file.target_ship,
                                target_app: &process_name,
                            },
                        );
                    let their_file_transfer = bindings::WitProcessNode {
                        node: &get_file.target_ship,
                        process: &process_name,
                    };

                    //  TODO: error handle
                    let _ = process_lib::yield_and_await_response(
                        our_name,
                        "filesystem",
                        Some(FileSystemRequest {
                            uri_string: get_file.uri_string.clone(),
                            action: FileSystemAction::Close(FileSystemMode::Append),
                        }),
                        None,
                    )?;

                    //  TODO: error handle
                    let message = process_lib::yield_and_await_response(
                        our_name,
                        "filesystem",
                        Some(FileSystemRequest {
                            uri_string: get_file.uri_string.clone(),
                            action: FileSystemAction::Open(FileSystemMode::AppendOverwrite),
                        }),
                        None,
                    )?;

                    //  TODO: error handle
                    let message = process_lib::yield_and_await_response(
                        &get_file.target_ship,
                        process_name,
                        Some(&FileTransferRequest::Start(FileTransferStart {
                            uri_string: get_file.uri_string.clone(),
                            chunk_size: get_file.chunk_size.clone(),
                        })),
                        None,
                    )?;

                    let FileTransferResponse::Started(metadata) =
                            process_lib::parse_message_json(message.payload.json)? else {
                        return Err(anyhow::anyhow!("expected Response type Started"));
                    };

                    let mut downloading = Downloading {
                        metadata,
                        received_pieces: vec![],
                    };

                    let mut piece_number = 0;
                    loop {
                        let message = process_lib::yield_and_await_response(
                            &get_file.target_ship,
                            process_name,
                            Some(&FileTransferRequest::GetPiece(FileTransferGetPiece {
                                uri_string: get_file.uri_string.clone(),
                                chunk_size: get_file.chunk_size.clone(),
                                piece_number,
                            })),
                            None,
                        )?;

                        let FileTransferResponse::FilePiece(file_piece) =
                                process_lib::parse_message_json(message.payload.json)? else {
                            return Err(anyhow::anyhow!("expected Response type FilePiece"));
                        };

                        if get_file.uri_string != file_piece.uri_string {
                            panic!("file_transfer: GetPiece wrong uri_string");
                        }
                        if downloading.received_pieces.len() != piece_number as usize {
                            panic!("file_transfer: GetPiece wrong piece_number");
                        }

                        let Some(bytes) = message.payload.bytes else {
                            return Err(anyhow::anyhow!(
                                "GetPiece: no bytes",
                            ));
                        };

                        //  TODO: handle errors
                        let _ = process_lib::yield_and_await_response(
                            our_name,
                            "filesystem",
                            Some(FileSystemRequest {
                                uri_string: file_piece.uri_string,
                                action: FileSystemAction::Append,
                            }),
                            Some(bytes),
                        )?;

                        print_to_terminal(format!(
                            "file_transfer: appended",
                        ).as_str());

                        piece_number += 1;

                        if downloading.metadata.number_pieces == piece_number {
                            //  received last piece; confirm file is good
                            let message = process_lib::yield_and_await_response(
                                our_name,
                                "filesystem",
                                Some(&FileSystemRequest {
                                    uri_string: get_file.uri_string.clone(),
                                    action: FileSystemAction::GetMetadata,
                                }),
                                None,
                            )?;

                            let FileSystemResponse::GetMetadata(file_metadata) =
                                    process_lib::parse_message_json(message.payload.json)? else {
                                return Err(anyhow::anyhow!("expected Response type GetMetadata"));
                            };

                            if Some(downloading.metadata.hash) == file_metadata.hash {
                                //  file is good; clean up

                                //  TODO: error handle
                                let _ = process_lib::yield_and_await_response(
                                    our_name,
                                    "filesystem",
                                    Some(&FileSystemRequest {
                                        uri_string: get_file.uri_string.clone(),
                                        action: FileSystemAction::Close(FileSystemMode::Append),
                                    }),
                                    None,
                                )?;


                                print_to_terminal(format!(
                                    "file_transfer: successfully downloaded {} from {}",
                                    get_file.uri_string,
                                    get_file.target_ship,
                                ).as_str());

                                process_lib::yield_one_request(
                                    false,
                                    &get_file.target_ship,
                                    &process_name,
                                    Some(FileTransferRequest::Done {
                                        uri_string: get_file.uri_string.clone(),
                                    }),
                                    None,
                                    None::<FileTransferContext>,
                                )?;
                            }
                            return Ok(MessageHandledStatus::Done);
                        }
                        downloading.received_pieces.push(file_piece.piece_hash);
                    }


                },
                FileTransferRequest::Start(start) => {
                    print_to_terminal("Start");

                    let chunk_size = start.chunk_size;

                    let key =  FileTransferKey {
                        requester: message.wire.source_ship,
                        server: our_name.into(),
                        uri_string: start.uri_string.clone(),
                    };

                    let message = process_lib::yield_and_await_response(
                        our_name,
                        "filesystem",
                        Some(&FileSystemRequest {
                            uri_string: start.uri_string.clone(),
                            action: FileSystemAction::GetMetadata,
                        }),
                        None,
                    )?;

                    let FileSystemResponse::GetMetadata(file_metadata) =
                            process_lib::parse_message_json(message.payload.json)? else {
                        return Err(anyhow::anyhow!("expected Response type GetMetadata"));
                    };

                    if file_metadata.uri_string != start.uri_string {
                        //  TODO: back to panic?
                        // bail(
                        return Err(anyhow::anyhow!(
                            "GetMetadata Response non-matching uri_string",
                        ));
                    }

                    let number_pieces = div_round_up(
                        file_metadata.len,
                        chunk_size
                    ) as u32;
                    let Some(hash) = file_metadata.hash else {
                        // bail(
                        return Err(anyhow::anyhow!(
                            "GetMetadata did not get hash from fs",
                        ));
                    };
                    let metadata = FileTransferMetadata {
                        key: key.clone(),
                        hash,
                        chunk_size,
                        number_pieces,
                        number_bytes: file_metadata.len,
                    };
                    *uploading = Some(Uploading {
                        metadata: metadata.clone(),
                        number_sent_pieces: 0,
                    });

                    //  TODO: handle in case of errors
                    let _ = process_lib::yield_and_await_response(
                        our_name,
                        "filesystem",
                        Some(&FileSystemRequest {
                            uri_string: file_metadata.uri_string,
                            action: FileSystemAction::Open(
                                FileSystemMode::Read
                            ),
                        }),
                        None,
                    )?;

                    process_lib::yield_one_response(
                        Some(FileTransferResponse::Started(metadata)),
                        None,
                        None::<FileTransferContext>,
                    )?;
                },
                FileTransferRequest::GetPiece(get_piece) => {
                    print_to_terminal("GetPiece");

                    let key = FileTransferKey {
                        requester: message.wire.source_ship.clone(),
                        server: our_name.into(),
                        uri_string: get_piece.uri_string.clone(),
                    };

                    let Some(ref mut uploading) = uploading else {
                        panic!("file_transfer: GetPiece uploading must be set");
                    };

                    let message = process_lib::yield_and_await_response(
                        our_name,
                        "filesystem",
                        Some(&FileSystemRequest {
                            uri_string: get_piece.uri_string.clone(),
                            action: FileSystemAction::ReadChunkFromOpen(
                                get_piece.chunk_size,
                            ),
                        }),
                        None,
                    )?;

                    let FileSystemResponse::ReadChunkFromOpen(uri_hash) =
                            process_lib::parse_message_json(message.payload.json)? else {
                        return Err(anyhow::anyhow!("expected Response type ReadChunkFromOpen"));
                    };

                    if get_piece.uri_string != uri_hash.uri_string {
                        panic!("file_transfer: ReadChunkFromOpen wrong uri_string");
                    }

                    let Some(bytes) = message.payload.bytes else {
                        // bail(
                        return Err(anyhow::anyhow!(
                            "ReadChunkFromOpen: no bytes",
                        ));
                    };

                    if uploading.number_sent_pieces != get_piece.piece_number {
                        panic!(
                            "file_transfer: piece_number {} differs from state {}",
                            get_piece.piece_number,
                            uploading.number_sent_pieces,
                        );
                    }

                    uploading.number_sent_pieces += 1;

                    process_lib::yield_one_response(
                        Some(FileTransferResponse::FilePiece(FileTransferFilePiece {
                            uri_string: uri_hash.uri_string,
                            piece_number: get_piece.piece_number,
                            piece_hash: uri_hash.hash,
                        })),
                        Some(bytes),
                        None::<FileTransferContext>,
                    )?;
                },
                FileTransferRequest::Done { uri_string } => {
                    let _ = process_lib::yield_and_await_response(
                        our_name,
                        "filesystem",
                        Some(&FileSystemRequest {
                            uri_string: uri_string.clone(),
                            action: FileSystemAction::Close(FileSystemMode::Read),
                        }),
                        None,
                    )?;


                    print_to_terminal(format!(
                        "file_transfer: done transferring {} to {}",
                        uri_string,
                        message.wire.source_ship,
                    ).as_str());

                    return Ok(MessageHandledStatus::Done);
                },
            }
            return Ok(MessageHandledStatus::ReadyForNext);
        },
        WitMessageType::Response => {
            print_to_terminal("Response");

            if "filesystem" == message.wire.source_app {
                match process_lib::parse_message_json(message.payload.json)? {
                    FileSystemResponse::Error(error) => {
                        let context: FileTransferContext = serde_json::from_str(&context)?;

                        handle_fs_error(
                            error,
                            &our_name,
                            &process_name,
                            context.key,
                        );
                    },
                    _ => {
                        panic!("file_transfer: unexpected filesystem Response");
                    },
                }
            } else if process_name == message.wire.source_app {
                match process_lib::parse_message_json(message.payload.json)? {
                    _ => {
                        panic!("file_transfer: unexpected file_transfer Response");
                    },
                }
            } else if "ws" == message.wire.source_app {
                let networking_error = process_lib::parse_message_json(message.payload.json)?;
                let context: FileTransferContext = serde_json::from_str(&context)?;

                handle_networking_error(
                    networking_error,
                    &our_name,
                    &process_name,
                    context.key,
                );
            }
            return Ok(MessageHandledStatus::ReadyForNext);
        },
    }
}

impl bindings::MicrokernelProcess for Component {
    fn run_process(our_name: String, process_name: String) {
        print_to_terminal("file_transfer_one_off: begin");

        let mut uploading: Option<Uploading> = None;

        loop {
            let (message, context) = bindings::await_next_message();
            match handle_message(
                message,
                context,
                &mut uploading,
                &our_name,
                &process_name,
            ) {
                Ok(status) => {
                    match status {
                        MessageHandledStatus::ReadyForNext => {},
                        MessageHandledStatus::Done => {
                            return;
                        },
                    }
                },
                Err(e) => {
                    //  TODO: should bail / Cancel
                    print_to_terminal(format!(
                        "{}: error: {:?}",
                        process_name,
                        e,
                    ).as_str());
                },
            };
        }
    }
}

bindings::export!(Component);
