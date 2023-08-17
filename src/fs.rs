use blake3::Hasher;
use bytes::Bytes;
use http::Uri;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::sync::RwLock;

use crate::types::*;

const HASH_READER_CHUNK_SIZE: usize = 1_024;
const CHUNK_SIZE: u64 = 100 * 1024; // 100kb

const SEPARATOR: [u8; 8] = [0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE, 0xBA, 0xBE];

//  On-Disk
struct ChunkEntry {
    file_uuid: u128,
    chunk_range: (u64, u64),
    chunk_hash: [u8; 32],
    data: Vec<u8>,
}

//  flags can be added, along with Vec<Backups>
//  enum ChunkEntryType { Backup, Chunk, ...}

// In-Memory
#[derive(Debug, Clone)]
struct InMemoryFile {
    filled_chunk_hash: Hasher, // filled chunk hash
    final_hash: Hasher, // start with filled_hash, apply partial ones to get final content addressed hash
    filled_chunks: Vec<MemoryChunk>,
    partial_chunk: Option<(MemoryChunk, Vec<u8>)>, // partial chunk data in-mem
}

// note on in-memory chunks. for future, might be useful to store them in a Btreemap of <position, chunkdata>

#[derive(Debug, Clone)]
struct MemoryChunk {
    chunk_range: (u64, u64), // start and end positions in the file
    chunk_hash: [u8; 32],
    wal_position: u64, // position of this chunk in the WAL.
}

#[derive(Serialize, Deserialize)]
pub enum FsAction {
    Write,
    Append(u128),
    Read(u128),
    ReadChunk(ReadChunkRequest),
    // different backup add/remove requests
}

#[derive(Serialize, Deserialize)]
pub enum FsResponse {
    //  bytes are in payload_bytes, file_id as u128 instead of string
    Read(u128),
    ReadChunk(u128),
    Write(u128),
    Append(u128),
    //  use FileSystemError
}

#[derive(Serialize, Deserialize)]
pub struct ReadChunkRequest {
    file: u128,
    start: u64,
    length: u64,
}

fn make_error_message(
    our_name: String,
    id: u64,
    source_process: String,
    error: FileSystemError,
) -> WrappedMessage {
    WrappedMessage {
        id,
        target: ProcessNode {
            node: our_name.clone(),
            process: source_process,
        },
        rsvp: None,
        message: Err(UqbarError {
            source: ProcessNode {
                node: our_name,
                process: "filesystem".into(),
            },
            timestamp: get_current_unix_time().unwrap(), //  TODO: handle error?
            content: UqbarErrorContent {
                kind: error.kind().into(),
                // message: format!("{}", error),
                message: serde_json::to_value(error).unwrap(), //  TODO: handle error?
                context: serde_json::to_value("").unwrap(),
            },
        }),
    }
}

pub async fn fs_sender(
    our_name: String,
    home_directory_path: String,
    send_to_loop: MessageSender,
    send_to_terminal: PrintSender,
    mut recv_in_fs: MessageReceiver,
) {
    // fs bootstrapping, create home_directory and log file if none.

    if let Err(e) = create_dir_if_dne(&home_directory_path).await {
        panic!("{}", e);
    }
    let home_directory_path = fs::canonicalize(home_directory_path).await.unwrap();
    let home_directory_path = home_directory_path.to_str().unwrap();

    //  open log file, load it in.
    let log_file_path = format!("{}/log.bin", home_directory_path_str);

    let log_file = fs::OpenOptions::new()
        .append(true)
        .read(true)
        .create(true)
        .open(&log_file_path)
        .await
        .expect("fs: failed to open log file");

    let mut manifest: HashMap<u128, Metadata> = HashMap::new();

    let _ = load_wal(&log_file, &mut manifest)
        .await
        .expect("fs: loading in log failed");

    let log_file = Arc::new(RwLock::new(log_file));

    let manifest = Arc::new(RwLock::new(manifest));

    while let Some(wrapped_message) = recv_in_fs.recv().await {
        let WrappedMessage { ref id, target: _, rsvp: _, message: Ok(Message { ref source, content: _ }), }
                = wrapped_message else {
            panic!("filesystem: unexpected Error")  //  TODO: implement error handling
        };

        let source_process = &source.process;
        if our_name != source.node {
            println!(
                "filesystem: request must come from our_name={}, got: {}",
                our_name, &wrapped_message,
            );
            continue;
        }

        let our_name = our_name.clone();
        let home_directory_path = home_directory_path.to_string();
        let source_process = source_process.into();
        let id = id.clone();
        let send_to_loop = send_to_loop.clone();
        let send_to_terminal = send_to_terminal.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_request(
                our_name.clone(),
                home_directory_path,
                wrapped_message,
                log_file,
                send_to_loop.clone(),
                send_to_terminal,
            )
            .await
            {
                send_to_loop
                    .send(make_error_message(our_name.into(), id, source_process, e))
                    .await
                    .unwrap();
            }
        });
    }
}

//  TODO: error handling: send error messages to caller
async fn handle_request(
    our_name: String,
    home_directory_path: String,
    wrapped_message: WrappedMessage,
    log_file: Arc<RwLock<fs::File>>,
    send_to_loop: MessageSender,
    send_to_terminal: PrintSender,
) -> Result<(), FileSystemError> {
    let WrappedMessage { id, target: _, rsvp, message: Ok(Message { source, content }), }
            = wrapped_message else {
        panic!("filesystem: unexpected Error")  //  TODO: implement error handling
    };
    let Some(value) = content.payload.json.clone() else {
        return Err(FileSystemError::BadJson {
            json: content.payload.json,
            error: "missing payload".into(),
        })
    };

    let MessageType::Request(is_expecting_response) = content.message_type else {
        return Err(FileSystemError::BadJson {
            json: content.payload.json,
            error: "not a Request".into(),
        })
    };

    let action: FsAction = match serde_json::from_value(value) {
        Ok(r) => r,
        Err(e) => {
            return Err(FileSystemError::BadJson {
                json: content.payload.json,
                error: format!("parse failed: {:?}", e),
            })
        }
    };

    let response_payload = match action {
        FsAction::Read(file_uuid) => {
            let manifest_read = manifest.read().await;

            // Get the InMemoryFile structure for the UUID
            let memfile = manifest_read.get(&file_uuid).unwrap();

            let mut result_data = Vec::new();

            // Loop through the filled_chunks and read relevant chunks
            for memory_chunk in &memfile.filled_chunks {
                if memory_chunk.chunk_range.1 > range.0 && memory_chunk.chunk_range.0 < range.1 {
                    // Use read lock
                    let mut log_file_lock = log_file.read().await;
                    let chunk_data =
                        get_chunk_data(memory_chunk.wal_position, &mut *log_file_lock).await?;

                    let start_idx = if memory_chunk.chunk_range.0 < range.0 {
                        (range.0 - memory_chunk.chunk_range.0) as usize
                    } else {
                        0
                    };
                    let end_idx = if memory_chunk.chunk_range.1 > range.1 {
                        (range.1 - memory_chunk.chunk_range.0) as usize
                    } else {
                        chunk_data.len()
                    };

                    result_data.extend_from_slice(&chunk_data[start_idx..end_idx]);
                }
            }

            if let Some((partial_chunk, _)) = &memfile.partial_chunk {
                if partial_chunk.chunk_range.1 > range.0 && partial_chunk.chunk_range.0 < range.1 {
                    let mut log_file_lock = log_file.read().await; // Use read lock
                    let chunk_data =
                        get_chunk_data(partial_chunk.wal_position, &mut *log_file_lock).await?;

                    let start_idx = if partial_chunk.chunk_range.0 < range.0 {
                        (range.0 - partial_chunk.chunk_range.0) as usize
                    } else {
                        0
                    };
                    let end_idx = if partial_chunk.chunk_range.1 > range.1 {
                        (range.1 - partial_chunk.chunk_range.0) as usize
                    } else {
                        chunk_data.len()
                    };

                    result_data.extend_from_slice(&chunk_data[start_idx..end_idx]);
                }
            }

            Payload {
                json: Some(
                    serde_json::to_value(FileSystemResponse::Read(FileSystemUriHash {
                        uri_string: request.uri_string,
                        hash,
                    }))
                    .unwrap(),
                ),
                bytes: Some(result_data),
            }
        }
        FsAction::Write => {
            let Some(data) = content.payload.bytes.clone() else {
                return Err(FileSystemError::BadBytes { action: "Write".into() })
            };

            let mut manifest = manifest.lock().await;

            let file_uuid = Uuid::new_v4().as_u128();

            let mut offset = 0;
            let mut filled_chunk_hash = Hasher::new();

            // fill up whole chunks first.
            while offset + CHUNK_SIZE <= data.len() {
                let chunk_data = &data[offset..offset + CHUNK_SIZE];
                let chunk_hash = Blake3::digest(chunk_data);
                filled_chunk_hash.update(chunk_data);

                let chunk_entry = ChunkEntry {
                    file_uuid,
                    chunk_range: (offset as u64, (offset + CHUNK_SIZE) as u64),
                    chunk_hash: *chunk_hash.as_bytes(),
                    data: chunk_data.to_vec(),
                };

                // Serialize and write to WAL
                let wal_position = wal_file.seek(SeekFrom::Current(0)).await?;
                let serialized_chunk = bincode::serialize(&chunk_entry)?;

                let mut buffer = Vec::new();
                buffer.extend_from_slice(&SEPARATOR);
                buffer.extend_from_slice(&(serialized_chunk.len() as u64).to_le_bytes());
                buffer.extend_from_slice(&serialized_chunk);

                wal_file.write_all(&buffer).await?;

                // Update in-memory structures for full chunks
                let memory_chunk = MemoryChunk {
                    chunk_range: (offset as u64, (offset + CHUNK_SIZE) as u64),
                    chunk_hash: *chunk_hash.as_bytes(),
                    wal_position,
                };

                let memfile = manifest.entry(file_uuid).or_insert_with(|| InMemoryFile {
                    filled_chunk_hash: Hasher::new(),
                    final_hash: Hasher::new(),
                    filled_chunks: Vec::new(),
                    partial_chunk: None,
                });

                memfile.filled_chunks.push(memory_chunk);
                offset += CHUNK_SIZE;
            }

            let mut final_hash = filled_chunk_hash.clone();

            // Handling partial bytes
            if offset < data.len() {
                let chunk_data = &data[offset..];

                let chunk_hash = Blake3::digest(chunk_data);
                final_hash.update(chunk_data);

                let chunk_entry = ChunkEntry {
                    file_uuid,
                    chunk_range: (offset as u64, data.len() as u64),
                    chunk_hash: *chunk_hash.as_bytes(),
                    data: chunk_data.to_vec(),
                };

                // Serialize and write to WAL
                let serialized_chunk = bincode::serialize(&chunk_entry)?;
                let wal_position = wal_file.seek(SeekFrom::Current(0)).await?;

                let mut buffer = Vec::new();
                buffer.extend_from_slice(&SEPARATOR);
                buffer.extend_from_slice(&(serialized_chunk.len() as u64).to_le_bytes());
                buffer.extend_from_slice(&serialized_chunk);

                wal_file.write_all(&buffer).await?;

                // Update in-memory structures for partial chunk
                let memory_chunk = MemoryChunk {
                    chunk_range: (offset as u64, data.len() as u64),
                    chunk_hash: *chunk_hash.as_bytes(),
                    wal_position,
                };

                let memfile = manifest.get_mut(&file_uuid).unwrap();
                memfile.partial_chunk = Some((memory_chunk, chunk_data.to_vec()));
            }

            Payload {
                json: Some(serde_json::to_value(FsResponse::Write(file_uuid)).unwrap()),
                bytes: None,
            }
        }
        FsAction::Append(file_uuid) => {
            let mut manifest_write = manifest.write().await;

            // Check if the given file UUID exists in the manifest.
            let in_memory_file = manifest_write.get_mut(&file_uuid).ok_or(FileSystemError::FileNotFound)?;
        
            // Serialize the chunk entry.
            let chunk_entry = ChunkEntry { data: data.clone() };
            let serialized_entry = bincode::serialize(&chunk_entry).map_err(FileSystemError::SerializeError)?;
        
            // Lock the WAL file for writing.
            let mut log_file_write = log_file.write().await;
        
            // Seek to the end to append data.
            let current_position = log_file_write.seek(SeekFrom::End(0)).await.map_err(FileSystemError::IOError)?;
        
            // Write the length of the serialized data followed by the actual serialized data.
            let length = serialized_entry.len() as u64;
            log_file_write.write_all(&length.to_be_bytes()).await.map_err(FileSystemError::IOError)?;
            log_file_write.write_all(&serialized_entry).await.map_err(FileSystemError::IOError)?;
        
            // Update the in-memory file's metadata.
            let last_position = if let Some((last_chunk, _)) = &in_memory_file.partial_chunk {
                last_chunk.chunk_range.1
            } else {
                0
            };
        
            let new_partial_chunk = MemoryChunk {
                chunk_range: (last_position, last_position + data.len() as u64),
                wal_position: current_position,
            };
        
            in_memory_file.partial_chunk = Some((new_partial_chunk, false));

            Payload {
                json: Some(serde_json::to_value(FsResponse::Append(file_uuid)).unwrap()),
                bytes: None,
            }
        }
    };

    if is_expecting_response {
        let response = WrappedMessage {
            id,
            target: ProcessNode {
                node: our_name.clone(),
                process: source.process.clone(),
            },
            rsvp,
            message: Ok(Message {
                source: ProcessNode {
                    node: our_name.clone(),
                    process: "filesystem".into(),
                },
                content: MessageContent {
                    message_type: MessageType::Response,
                    payload: response_payload,
                },
            }),
        };

        let _ = send_to_loop.send(response).await;
    }

    Ok(())
}

/// HELPERS

async fn load_wal(
    log_file: &fs::File,
    manifest: &mut HashMap<u128, InMemoryFile>,
) -> Result<(), FileSystemError> {
    let mut current_position = 0;

    loop {
        // Seek to the current position
        log_file.seek(SeekFrom::Start(current_position)).await?;

        // Check for the SEPARATOR
        let mut sep_buffer = [0u8; SEPARATOR.len()];
        let separator_size = log_file.read(&mut sep_buffer).await?;

        if separator_size == 0 || sep_buffer != SEPARATOR {
            break; // End of file or corruption
        }

        // Read length of the serialized entry
        let mut length_buffer = [0u8; 8];
        log_file.read_exact(&mut length_buffer).await?;
        let entry_length = u64::from_be_bytes(length_buffer) as usize;

        // Read serialized entry
        let mut entry_buffer = vec![0u8; entry_length];
        let read_size = log_file.read(&mut entry_buffer).await?;

        if read_size == 0 {
            break; // End of file
        }

        let chunk_entry: Result<ChunkEntry, _> = bincode::deserialize(&entry_buffer);

        match chunk_entry {
            Ok(entry) => {
                // Process the valid ChunkEntry
                if entry.chunk_range.1 - entry.chunk_range.0 == CHUNK_SIZE as u64 {
                    let memfile = manifest
                        .entry(entry.file_uuid)
                        .or_insert_with(|| InMemoryFile {
                            filled_chunk_hash: Hasher::new(),
                            final_hash: Hasher::new(),
                            filled_chunks: Vec::new(),
                            partial_chunk: None,
                        });

                    memfile.filled_chunk_hash.update(&entry.data);

                    let memory_chunk = MemoryChunk {
                        chunk_range: entry.chunk_range,
                        chunk_hash: entry.chunk_hash,
                        wal_position: current_position,
                    };

                    memfile.filled_chunks.push(memory_chunk);
                } else {
                    // Handle partial chunks
                    let memfile = manifest
                        .entry(entry.file_uuid)
                        .or_insert_with(InMemoryFile::default);

                    // Take any existing partial data and append the new data
                    let existing_partial_data = memfile
                        .partial_chunk
                        .take()
                        .map_or_else(Vec::new, |(_, data)| data);
                    let combined_data = [existing_partial_data, entry.data.clone()].concat();

                    // Check if combined_data forms a full chunk or remains partial
                    if combined_data.len() >= CHUNK_SIZE {
                        // Update the filled_chunk_hash and add to filled_chunks
                        memfile
                            .filled_chunk_hash
                            .update(&combined_data[0..CHUNK_SIZE]);

                        let memory_chunk = MemoryChunk {
                            chunk_range: (
                                entry.chunk_range.0,
                                entry.chunk_range.0 + CHUNK_SIZE as u64,
                            ),
                            chunk_hash: Blake3::digest(&combined_data[0..CHUNK_SIZE]),
                            wal_position: current_position,
                        };

                        memfile.filled_chunks.push(memory_chunk);

                        // If there are any remaining bytes, treat them as the new partial data
                        if combined_data.len() > CHUNK_SIZE {
                            memfile.final_hash.update(&combined_data[CHUNK_SIZE..]);

                            let memory_chunk = MemoryChunk {
                                chunk_range: (
                                    entry.chunk_range.0 + CHUNK_SIZE as u64,
                                    entry.chunk_range.0 + combined_data.len() as u64,
                                ),
                                chunk_hash: Blake3::digest(&combined_data[CHUNK_SIZE..]),
                                wal_position: current_position,
                            };

                            memfile.partial_chunk =
                                Some((memory_chunk, combined_data[CHUNK_SIZE..].to_vec()));
                        }
                    } else {
                        // If data remains partial after combining, simply update the final_hash and store as partial_chunk
                        memfile.final_hash.update(&combined_data);

                        let memory_chunk = MemoryChunk {
                            chunk_range: entry.chunk_range,
                            chunk_hash: entry.chunk_hash,
                            wal_position: current_position,
                        };

                        memfile.partial_chunk = Some((memory_chunk, combined_data));
                    }
                }

                // Adjust the current position, +8 is for initial entry_length
                current_position += (SEPARATOR.len() + 8 + entry_length) as u64;
            }
            Err(_) => {
                // corrupted Entry...
                // currently we skip to the next bytes, but could SLOWLY iterate until we find next separator.
                current_position += (SEPARATOR.len() + 8 + entry_length) as u64;
            }
        }
    }

    Ok(())
}

//  option: store direct positions of the actual data instead.
async fn get_chunk_data(
    wal_position: u64,
    log_file: &mut fs::File,
) -> Result<Vec<u8>, FileSystemError> {
    log_file.seek(SeekFrom::Start(wal_position)).await?;

    let mut length_buffer = [0u8; 8];
    log_file.read_exact(&mut length_buffer).await?;
    let entry_length = u64::from_be_bytes(length_buffer);

    let mut entry_buffer = vec![0u8; entry_length as usize];
    log_file.read_exact(&mut entry_buffer).await?;

    let chunk_entry: ChunkEntry = bincode::deserialize(&entry_buffer)?;
    Ok(chunk_entry.data)
}

// this one should be made more efficient:
// in the case of a corrupt WAL entry, we seek until we find the next entry separator.
// unused for now.
async fn seek_to_next_separator(log_file: &mut fs::File) -> Result<u64, std::io::Error> {
    let mut buffer = [0u8; SEPARATOR.len()];
    let mut current_position = log_file.seek(SeekFrom::Current(0)).await?;

    loop {
        let read_size = log_file.read(&mut buffer).await?;

        // End of file or insufficient bytes to match the SEPARATOR
        if read_size < SEPARATOR.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Could not find the next separator",
            ));
        }

        if buffer == SEPARATOR {
            return Ok(current_position);
        } else {
            current_position += 1;
            log_file.seek(SeekFrom::Start(current_position)).await?;
        }
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
    source_process: &str,
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

async fn get_file_bytes_left(file_path: &str, file: &mut fs::File) -> Result<u64, FileSystemError> {
    let current_pos = match file.stream_position().await {
        Ok(p) => p,
        Err(e) => {
            return Err(FileSystemError::FsError {
                what: "reading current stream position".into(),
                path: file_path.into(),
                error: format!("{}", e),
            })
        }
    };
    let metadata = match file.metadata().await {
        Ok(m) => m,
        Err(e) => {
            return Err(FileSystemError::FsError {
                what: "reading metadata".into(),
                path: file_path.into(),
                error: format!("{}", e),
            })
        }
    };

    Ok(metadata.len() - current_pos)
}

//  TODO: factor our with microkernel
fn get_current_unix_time() -> anyhow::Result<u64> {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(t) => Ok(t.as_secs()),
        Err(e) => Err(e.into()),
    }
}
