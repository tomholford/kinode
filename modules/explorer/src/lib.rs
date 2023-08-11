cargo_component_bindings::generate!();

use anyhow::anyhow;
use std::collections::HashMap;
use serde::{Serialize, Deserialize};
use thiserror::Error;
use bindings::print_to_terminal;
use bindings::component::microkernel_process::types::WitPayload;
use bindings::component::microkernel_process::types::WitProcessNode;
use bindings::component::microkernel_process::types::WitProtomessageType;
use bindings::component::microkernel_process::types::WitRequestTypeWithTarget;
use bindings::component::microkernel_process::types::WitUqbarError;

struct Component;

type FileHash = [u8; 32];
type HierarchicalFS = HashMap<String, FileHash>; // TODO this should be a trie

// TYPES COPY/PASTE START
// #[derive(Debug, Serialize, Deserialize)]
impl FileSystemError {
    pub fn kind(&self) -> &str {
        match *self {
            FileSystemError::BadUri { .. } => "BadUri",
            FileSystemError::BadJson { .. } =>  "BadJson",
            FileSystemError::BadBytes { .. } => "BadBytes",
            FileSystemError::IllegalAccess { .. } => "IllegalAccess",
            FileSystemError::AlreadyOpen { .. } => "AlreadyOpen",
            FileSystemError::NotCurrentlyOpen { .. } => "NotCurrentlyOpen",
            FileSystemError::BadPathJoin { .. } => "BadPathJoin",
            FileSystemError::CouldNotMakeDir { .. } => "CouldNotMakeDir",
            FileSystemError::ReadFailed { .. } => "ReadFailed",
            FileSystemError::WriteFailed { .. } => "WriteFailed",
            FileSystemError::OpenFailed { .. } => "OpenFailed",
            FileSystemError::FsError { .. } => "FsError",
        }
    }
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
// TYPES COPY/PASTE END

const UPLOAD_PAGE: &str = include_str!("upload.html");

fn yield_write(
    our_name: &str,
    uri_string: String,
    bytes: Vec<u8>,
    context: &str,
) {
    bindings::yield_results(Ok(vec![
        (
            bindings::WitProtomessage {
                protomessage_type: WitProtomessageType::Request(
                    WitRequestTypeWithTarget {
                        is_expecting_response: true,
                        target: WitProcessNode {
                            node: our_name.into(),
                            process: "filesystem".into(),
                        },
                    },
                ),
                payload: WitPayload {
                    json: Some(serde_json::to_string(
                        &FileSystemRequest {
                            uri_string,
                            action: FileSystemAction::Write,
                        }
                    ).unwrap()),
                    bytes: Some(bytes),
                },
            },
            context.into(),
        ),
    ].as_slice()));
}

fn handle_next_message(
    file_names: &mut HierarchicalFS,
    our_name: &str,
    process_name: &str,
) -> anyhow::Result<()> {
    let (message, context) = bindings::await_next_message()?;
    let Some(ref payload_json_string) = message.content.payload.json else {
        return Err(anyhow!("require non-empty json payload"));
    };

    print_to_terminal(
        0,
        format!("{}: got json {}", &process_name, payload_json_string).as_str()
    );

    let message_from_loop: serde_json::Value = serde_json::from_str(&payload_json_string).unwrap();
    if message_from_loop["method"] == "GET" && message_from_loop["raw_path"] == "/apps/explorer/upload" {
        bindings::yield_results(Ok(vec![(
            bindings::WitProtomessage {
                protomessage_type: WitProtomessageType::Response,
                payload: WitPayload {
                    json: Some(serde_json::json!({
                        "action": "response",
                        "status": 200,
                        "headers": {
                            "Content-Type": "text/html",
                        },
                    }).to_string()),
                    bytes: Some(UPLOAD_PAGE.replace("${our}", &our_name).as_bytes().to_vec())
                }
            },
            "".into(),
        )].as_slice()));
        return Ok(());
    } else if message_from_loop["method"] == "POST" && message_from_loop["raw_path"] == "/apps/explorer/upload" {
        bindings::print_to_terminal(0, "got upload request");
        let body_bytes = message.content.payload.bytes.unwrap_or(vec![]); // TODO no unwrap
        bindings::print_to_terminal(0, format!("len {:?}", body_bytes.len()).as_str());
        // TODO I have to decode body_bytes somehow? Bytes aren't exactly correct...
        // TODO prompt user for file name before saving
        yield_write(&our_name, "fs://zebra.jpg".to_string(), body_bytes, "");
        bindings::yield_results(Ok(vec![(
            bindings::WitProtomessage {
                protomessage_type: WitProtomessageType::Response,
                payload: WitPayload {
                    json: Some(serde_json::json!({
                        "action": "response",
                        "status": 200,
                        "headers": {
                            "Content-Type": "text/html",
                        },
                    }).to_string()),
                    bytes: Some("success".as_bytes().to_vec())
                }
            },
            "".into(),
        )].as_slice()));
        return Ok(());
    } else {
        return Err(anyhow!("unrecognized request"));
    }
}

impl bindings::MicrokernelProcess for Component {
    fn run_process(our_name: String, process_name: String) {
        print_to_terminal(1, "file_transfer: begin");
        // HTTP bindings
        bindings::yield_results(Ok(
            vec![(
                bindings::WitProtomessage {
                    protomessage_type: WitProtomessageType::Request(
                        WitRequestTypeWithTarget {
                            is_expecting_response: false,
                            target: WitProcessNode {
                                node: our_name.clone(),
                                process: "http_bindings".into(),
                            },
                        }
                    ),
                    payload: WitPayload {
                        json: Some(serde_json::json!({
                            "action": "bind-app",
                            "path": "/apps/explorer/upload",
                            "app": process_name
                        }).to_string()),
                        bytes: None
                    }
                },
                "".into(),
            ), (
                bindings::WitProtomessage {
                    protomessage_type: WitProtomessageType::Request(
                        WitRequestTypeWithTarget {
                            is_expecting_response: false,
                            target: WitProcessNode {
                                node: our_name.clone(),
                                process: "http_bindings".into(),
                            },
                        }
                    ),
                    payload: WitPayload {
                        json: Some(serde_json::json!({
                            "action": "bind-app",
                            "path": "/apps/explorer/file/:path",
                            "app": process_name
                        }).to_string()),
                        bytes: None
                    }
                },
                "".into(),
            )].as_slice()
        ));

        let mut file_names: HierarchicalFS = HashMap::new();

        loop {
            match handle_next_message(
                &mut file_names,
                &our_name,
                &process_name,
            ) {
                Ok(_) => {},
                Err(e) => {
                    match e.downcast_ref::<WitUqbarError>() {
                        None => print_to_terminal(0, format!("{}: error: {:?}", process_name, e).as_str()),
                        Some(uqbar_error) => {
                            // TODO handle afs errors here
                            print_to_terminal(
                                0,
                                format!("{}: error: {:?}", process_name, e).as_str()
                            )
                        },
                    }
                },
            }
        }
    }
}
