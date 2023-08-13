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

type FileHash = [u8; 32];
type HierarchicalFS = HashMap<String, FileHash>; // TODO this should be a trie

#[derive(Debug)]
struct File {
    name: String,
    content_type: Option<String>,
    data: Vec<u8>,
}

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

fn extract_boundary_from_headers(headers: &serde_json::Value) -> Option<String> {
    // TODO http-bindings or http-server should format all headers to Pascal case
    // because an http-server should be case insensitive. 
    let content_type = headers.get("Content-Type")?.as_str()?;
    if let Some(start) = content_type.find("boundary=") {
        let boundary = &content_type[start + "boundary=".len()..];
        return Some(boundary.to_string());
    }
    None
}

fn split_body(body: Vec<u8>, boundary: String) -> Vec<Vec<u8>> {
    let boundary_bytes = format!("\r\n--{}", boundary).into_bytes();

    let mut chunks = Vec::new();
    let mut last_index = boundary_bytes.len(); // Start after the first boundary

    for (index, window) in body.windows(boundary_bytes.len()).enumerate() {
        if window == boundary_bytes.as_slice() {
            if index > last_index {
                chunks.push(body[last_index..index].to_vec());
            }
            last_index = index + boundary_bytes.len();
        }
    }

    if last_index < body.len() {
        chunks.push(body[last_index..].to_vec());
    }

    // Remove the last boundary, which typically ends with "--"
    if let Some(last_chunk) = chunks.last_mut() {
        if last_chunk.ends_with(b"--\r\n") {
            last_chunk.truncate(last_chunk.len() - 4);
        }
    }

    // Remove any empty chunks or chunks containing only the boundary ending
    chunks.retain(|chunk| !chunk.is_empty() && chunk != &boundary_bytes);

    chunks
}

fn extract_file_from_chunk(chunk: Vec<u8>) -> Option<File> {
    let headers_end = b"\r\n\r\n";
    let headers_end_pos = chunk.windows(4).position(|window| window == headers_end)?;

    let headers_bytes = &chunk[..headers_end_pos];
    let data = chunk[headers_end_pos + 4..].to_vec();

    let headers_str = String::from_utf8_lossy(headers_bytes);
    
    let mut name = None;
    let mut content_type = None;

    let mut headers_iter = headers_str.lines().peekable();
    while headers_iter.peek().is_some() {
        let header = headers_iter.next()?;
        if header.contains("filename=") {
            if let Some(start) = header.find("filename=\"") {
                let rest = &header[start + "filename=\"".len()..];
                if let Some(end) = rest.find('"') {
                    name = Some(rest[..end].to_string());
                } else {
                    // Filename spans multiple headers, so we need to look in the subsequent headers
                    let mut complete_filename = rest.to_string();
                    while let Some(next_line) = headers_iter.peek() {
                        if next_line.contains("\"") {
                            complete_filename.push_str(&next_line[..next_line.find('"')?]);
                            break;
                        } else {
                            complete_filename.push_str(next_line);
                            headers_iter.next(); // Move to the next header
                        }
                    }
                    name = Some(complete_filename);
                }
            }
        } else if header.to_lowercase().starts_with("Content-Type:") {
            content_type = Some(header["Content-Type:".len()..].trim().to_string());
        }
    }
    
    Some(File {
        name: name?,
        content_type,
        data
    })
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
    if message_from_loop["method"] == "GET" && message_from_loop["raw_path"] == "/apps/explorer" {
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
    } else if message_from_loop["method"] == "POST" && message_from_loop["raw_path"] == "/apps/explorer" {
        let body_bytes = message.content.payload.bytes.unwrap_or(vec![]); // TODO no unwrap

        let boundary = extract_boundary_from_headers(&message_from_loop["headers"].clone()).unwrap();
        let files: Vec<Vec<u8>> = split_body(body_bytes, boundary);
        bindings::print_to_terminal(0, format!("files {:?}", files.len()).as_str());
        for file in files {
            let fil: File = extract_file_from_chunk(file).unwrap();

            // TODO replace this with a write to AFS instead of the normal FS
            yield_write(&our_name, format!("fs://{}", fil.name), fil.data, "");
            // Real file hash should be returned by the 
            let dummy_file_hash: FileHash = [0; 32];
            file_names.insert(fil.name, dummy_file_hash);
        }

        // TODO error handling
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
    } else if message_from_loop["method"] == "GET" && message_from_loop["raw_path"] == "/apps/explorer/files" {
        let mut files = vec![];
        for (name, hash) in file_names.iter() {
            files.push(name);
        }
        bindings::yield_results(Ok(vec![(
            bindings::WitProtomessage {
                protomessage_type: WitProtomessageType::Response,
                payload: WitPayload {
                    json: Some(serde_json::json!({
                        "action": "response",
                        "status": 200,
                        "headers": {
                            "Content-Type": "application/json",
                        },
                    }).to_string()),
                    bytes: Some(serde_json::to_vec(&files).unwrap())
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
                            "path": "/apps/explorer",
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
                            "path": "/apps/explorer/files",
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
