cargo_component_bindings::generate!();

use anyhow::anyhow;
use std::collections::HashMap;
use serde::{Serialize, Deserialize};

use bindings::{MicrokernelProcess, print_to_terminal, receive, send};
use bindings::component::microkernel_process::types;

mod process_lib;

struct Component;

// TYPES COPY/PASTE START
#[derive(Serialize, Deserialize, Debug)]
pub enum FsAction {
    Write,
    Append(Option<[u8; 32]>),
    Read([u8; 32]),
    ReadChunk(ReadChunkRequest),
    PmWrite,                     //  specific case for process manager persistance.
    Delete([u8; 32]),
    Length([u8; 32]),
}
#[derive(Serialize, Deserialize, Debug)]
pub struct ReadChunkRequest {
    file_hash: [u8; 32],
    start: u64,
    length: u64,
}
#[derive(Serialize, Deserialize, Debug)]
pub enum FsResponse {
    //  bytes are in payload_bytes
    Read([u8; 32]),
    ReadChunk([u8; 32]),
    Write([u8; 32]),
    Append([u8; 32]),
    Delete([u8; 32]),
    Length(u64),
    //  use FileSystemError
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

// TODO swap this out for an AFS call that returns the hash of the file
fn send_write(
    our_name: &str,
    bytes: Vec<u8>,
) -> anyhow::Result<[u8; 32]> {
    let file_msg = process_lib::send_and_await_receive(
        our_name.to_string(),
        types::ProcessIdentifier::Name("fs".to_string()),
        Some(&FsAction::Write),
        types::OutboundPayloadBytes::Some(bytes),
    )?;
    let json = match file_msg {
        Err(e) => panic!("explorer: error while writing: {:?}", e),
        Ok(m) => process_lib::get_json(&m),
    }?;
    match process_lib::parse_message_json(Some(json))? {
        FsResponse::Write(file_hash) => Ok(file_hash),
        _ => Err(anyhow!("unexpected Response to Write")),
    }
}

// TODO swap this out for an AFS read that returns the file bytes
fn send_read(
    our_name: &str,
    file_hash: [u8; 32],
) -> anyhow::Result<Vec<u8>> {
    let file_msg = process_lib::send_and_await_receive(
        our_name.to_string(),
        types::ProcessIdentifier::Name("fs".to_string()),
        Some(&FsAction::Read(file_hash)),
        types::OutboundPayloadBytes::None,
    )?;
    match file_msg {
        // Err(e) => Err(e),
        Err(e) => panic!("explorer: error while reading: {:?}", e),
        Ok(m) => process_lib::get_bytes(m),
    }
}

fn extract_boundary_from_headers(headers: &serde_json::Value) -> Option<String> {
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
    our: &types::ProcessAddress,
) -> anyhow::Result<()> {
    let (message, _context) = receive()?;

    let types::InboundMessage::Request(types::InboundRequest {
        is_expecting_response: _,
        payload: types::InboundPayload {
            source: _,
            ref json,
            bytes,
        },
    }) = message else {
        return Err(anyhow!("got unexpected response"));
    };

    let Some(json) = json else {
        panic!("bar");
    };
    let message_from_loop: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => {
            return Err(anyhow!("failed to parse json"));
        },
    };

    print_to_terminal(
        1,
        &format!("explorer: got json {}", message_from_loop),
    );

    if (message_from_loop["method"] == serde_json::Value::Null) & (message_from_loop["Initialize"] == serde_json::Value::Null) {
        let bytes = match bytes {
            types::InboundPayloadBytes::Some(bytes) => Ok(bytes),
            _ => Err(anyhow!("bytes field is not Some")),
        }?;
        *file_names = bincode::deserialize(&bytes[..])?;
        process_lib::send_response(
            None::<String>,
            types::OutboundPayloadBytes::None,
            "".into(),
        )?;
        return Ok(());
    } else if message_from_loop["method"] == "GET" && message_from_loop["path"] == "/explorer" {
        process_lib::send_response(
            Some(serde_json::json!({
                "action": "response",
                "status": 200,
                "headers": {
                    "Content-Type": "text/html",
                },
            })),
            types::OutboundPayloadBytes::Some(
                UPLOAD_PAGE
                    .replace("${our}", &our.node)
                    .as_bytes()
                    .to_vec()
            ),
            "".into(),
        )?;
        return Ok(());
    } else if message_from_loop["method"] == "POST" && message_from_loop["path"] == "/explorer" {
        let body_bytes = match bytes {
            types::InboundPayloadBytes::Some(bytes) => Ok(bytes),
            _ => Err(anyhow!("bytes field is not Some")),
        }?;

        let boundary = extract_boundary_from_headers(&message_from_loop["headers"].clone()).unwrap();
        let files: Vec<Vec<u8>> = split_body(body_bytes, boundary);
        print_to_terminal(1, format!("files {:?}", files.len()).as_str());
        for file in files {
            let fil: File = extract_file_from_chunk(file).unwrap();

            let file_hash = send_write(&our.node, fil.data)?;
            file_names.insert(fil.name, file_hash);
        }
        let _ = process_lib::persist_state(&our.node, &file_names)?;

        // TODO error handling
        process_lib::send_response(
            Some(serde_json::json!({
                "action": "response",
                "status": 200,
                "headers": {
                    "Content-Type": "text/html",
                },
            })),
            types::OutboundPayloadBytes::Some("success".as_bytes().to_vec()),
            "".into(),
        )?;
        return Ok(());
    } else if message_from_loop["method"] == "GET" && message_from_loop["path"] == "/explorer/files" {
        let mut files = vec![];
        for (name, hash) in file_names.iter() {
            files.push(name);
        }
        process_lib::send_response(
            Some(serde_json::json!({
                "action": "response",
                "status": 200,
                "headers": {
                    "Content-Type": "application/json",
                },
            })),
            types::OutboundPayloadBytes::Some(serde_json::to_vec(&files).unwrap()),
            "".into(),
        )?;
        return Ok(());
    } else if message_from_loop["method"] == "GET" && message_from_loop["path"] == "/explorer/file/:file" {
        let file = message_from_loop["url_params"]["file"].as_str().unwrap();
        let file = percent_encoding::percent_decode_str(file)
            .decode_utf8_lossy()
            .into_owned();

        print_to_terminal(1, &format!("explorer: file_names: {:?}", file_names));

        if let Some(hash) = file_names.get(&file) {
            let file_bytes = send_read(&our.node, hash.clone())?;
            process_lib::send_response(
                Some(serde_json::json!({
                    "action": "response",
                    "status": 200,
                    "headers": {
                        // "Content-Type": "application/octet-stream",
                    },
                })),
                types::OutboundPayloadBytes::Some(file_bytes),
                "".into(),
            )?;
        } else {
            process_lib::send_response(
                Some(serde_json::json!({
                    "action": "response",
                    "status": 404,
                    "headers": {
                        "Content-Type": "text/html",
                    },
                })),
                types::OutboundPayloadBytes::Some("not found".as_bytes().to_vec()),
                "".into(),
            )?;
        }
        return Ok(());
    } else {
        print_to_terminal(0, format!("req {:?}", message_from_loop).as_str());
        return Err(anyhow!("unrecognized request"));
    }
}

impl MicrokernelProcess for Component {
    fn run_process(our: types::ProcessAddress) {
        print_to_terminal(1, "explorer: begin");
        // HTTP bindings
        let our_http_bindings = types::ProcessReference {
            node: our.node.clone(),
            identifier: types::ProcessIdentifier::Name("http_bindings".into()),
        };
        let process_name = match our.name {
            Some(ref name) => name.clone(),
            None => format!("{}", our.id),
        };
        send(Ok(&types::OutboundMessage::Requests(vec![
            (
                vec![
                    types::OutboundRequest {
                        is_expecting_response: false,
                        target: our_http_bindings.clone(),
                        payload: types::OutboundPayload {
                            json: Some(serde_json::json!({
                                "action": "bind-app",
                                "path": "/explorer",
                                "authenticated": true,
                                "app": process_name
                            }).to_string()),
                            bytes: types::OutboundPayloadBytes::None,
                        },
                    },
                    types::OutboundRequest {
                        is_expecting_response: false,
                        target: our_http_bindings.clone(),
                        payload: types::OutboundPayload {
                            json: Some(serde_json::json!({
                                "action": "bind-app",
                                "path": "/explorer/files",
                                "authenticated": true,
                                "app": process_name
                            }).to_string()),
                            bytes: types::OutboundPayloadBytes::None,
                        },
                    },
                    types::OutboundRequest {
                        is_expecting_response: false,
                        target: our_http_bindings.clone(),
                        payload: types::OutboundPayload {
                            json: Some(serde_json::json!({
                                "action": "bind-app",
                                "path": "/explorer/file/:file",
                                "authenticated": true,
                                "app": process_name
                            }).to_string()),
                            bytes: types::OutboundPayloadBytes::None,
                        },
                    },
                ],
                "".into(),
            ),
        ])));

        let mut file_names: HierarchicalFS = HashMap::new();

        loop {
            match handle_next_message(
                &mut file_names,
                &our,
            ) {
                Ok(_) => {},
                Err(e) => {
                    match e.downcast_ref::<types::UqbarError>() {
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
