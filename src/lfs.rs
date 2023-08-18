/// log structured filesystem
/// immutable/append []

use blake3::Hasher;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::fs;
use tokio::io::{self, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::sync::RwLock;
use uuid;
use bincode;

use crate::types::*;

// const CHUNK_SIZE: u64 = 100 * 1024; // 100kb
// const SEPARATOR: [u8; 8] = [0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE, 0xBA, 0xBE];

// On-Disk
#[derive(Serialize, Deserialize)]
struct ChunkEntry {
    file_uuid: u128,
    chunk_range: (u64, u64),
    chunk_hash: [u8; 32],
    //  data: Vec<u8>, data is kept separately after the chunk_entry
}

//  flags can be added, along with Vec<Backups>
//  enum ChunkEntryType { Backup, Chunk, ...}

// In-Memory
#[derive(Debug, Clone)]
struct InMemoryFile {
    hasher: Hasher,            // content addressed hash (naive)
    chunks: Vec<MemoryChunk>,  // chunks meta-information.
}

#[derive(Debug, Clone)]
struct MemoryChunk {
    chunk_range: (u64, u64),  // start and end positions in the file
    chunk_hash: [u8; 32],     // data hash by itself.
    wal_position: u64,        // position of this chunk's data in the WAL.
}

#[derive(Serialize, Deserialize, Debug)]
pub enum FsAction {
    Write,
    Append([u8; 32]),
    Read([u8; 32]),
    ReadChunk(ReadChunkRequest),
    PmWrite                  //  specific case for process manager persistance.
    // different backup add/remove requests
}

#[derive(Serialize, Deserialize, Debug)]
pub enum FsResponse {
    //  bytes are in payload_bytes, [old-fileHash, new_filehash, file_uuid]
    Read([u8; 32]),
    ReadChunk([u8; 32]),
    Write([u8; 32]),
    Append([u8; 32]),   //  new file_hash [old too?]
                        //  use FileSystemError
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ReadChunkRequest {
    file_hash: [u8; 32],
    start: u64,
    length: u64,
}

// special process manager uuid
const pm_uuid: u128 = 0;

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
    let home_directory_path: std::path::PathBuf = fs::canonicalize(home_directory_path).await.unwrap();

    //  open log file, load it in.

    let log_file_path = home_directory_path.join("log.bin");
    let mut log_file = fs::OpenOptions::new()
        .append(true)
        .read(true)
        .create(true)
        .open(&log_file_path)
        .await
        .expect("fs: failed to open log file");

    //  in memory details about files.
    let mut manifest: HashMap<u128, InMemoryFile> = HashMap::new();

    //  enable lookup by file_hash
    let mut hash_index: HashMap<[u8; 32], u128> = HashMap::new();

    load_wal(&mut log_file, &mut manifest, &mut hash_index)
        .await
        .expect("wal loading failed.");
    println!("whole manifest {:?}", manifest);

    let manifest: Arc<RwLock<HashMap<u128, InMemoryFile>>> = Arc::new(RwLock::new(manifest));
    let hash_index: Arc<RwLock<HashMap<[u8; 32], u128>>> = Arc::new(RwLock::new(hash_index));

    let log_file = Arc::new(RwLock::new(log_file));

    //  into main loop
    while let Some(wrapped_message) = recv_in_fs.recv().await {
        let WrappedMessage { ref id, target: _, rsvp: _, message: Ok(Message { ref source, content: _ }), }
                = wrapped_message else {
            println!(
                "fs: got weird message from {}: {}",
                our_name, &wrapped_message,
            );
            continue;
        };

        let source_process = &source.process;
        if our_name != source.node {
            println!(
                "fs: request must come from our_name={}, got: {}",
                our_name, &wrapped_message,
            );
            continue;
        }
        let log_clone = log_file.clone();
        let manifest_clone = manifest.clone();
        let hash_index_clone = hash_index.clone();

        let our_name = our_name.clone();
        let source_process = source_process.into();
        let id = id.clone();
        let send_to_loop = send_to_loop.clone();
        let send_to_terminal = send_to_terminal.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_request(
                our_name.clone(),
                wrapped_message,
                log_clone,
                manifest_clone,
                hash_index_clone,
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

async fn handle_request(
    our_name: String,
    wrapped_message: WrappedMessage,
    log: Arc<RwLock<fs::File>>,
    manifest: Arc<RwLock<HashMap<u128, InMemoryFile>>>,
    hash_index: Arc<RwLock<HashMap<[u8; 32], u128>>>,
    send_to_loop: MessageSender,
    send_to_terminal: PrintSender,
) -> Result<(), FileSystemError> {
    let WrappedMessage { id, target: _, rsvp, message: Ok(Message { source, content }), }
            = wrapped_message else {
        return Err(FileSystemError::LFSError { error: "got weird message".to_string() });
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
        FsAction::Write => {
            let Some(data) = content.payload.bytes.clone() else {
                return Err(FileSystemError::BadBytes { action: "Write".into() })
            };

            let file_uuid = uuid::Uuid::new_v4().as_u128();
            
            //  hashing: note chunks[]
            let mut hasher = blake3::Hasher::new();
            hasher.update(&data);
            let hash_result = hasher.finalize();
            let hash_array: [u8; 32] = *hash_result.as_bytes();
            
            // don't write the same file twice
            // TODO: reconsider, doesn't quite map with filehash => UUID, but should
            //  let hash_exists;
            //  {
            //      let whash_index = hash_index.read().await;
            //      hash_exists = whash_index.contains_key(&hash_array);
            //  }
            //  
            //  if hash_exists {
            //      Payload {
            //          json: Some(serde_json::to_value(FsResponse::Write(hash_array)).unwrap()),
            //          bytes: None,
            //      };
            //  }   

            let entry = ChunkEntry {
                file_uuid,
                chunk_range: (0, data.len() as u64 - 1),
                chunk_hash: hash_array.clone(),
            };

            let wal_result;
            {
                let mut wlog = log.write().await;
                wal_result = append_to_wal(&mut wlog, &entry, &data).await;
            }

            match wal_result {
                Ok(wal_position) => {
                    let mut wmanifest = manifest.write().await;
                    let mut whash_index = hash_index.write().await;

                    let memory_chunk = MemoryChunk {
                        chunk_range: entry.chunk_range,
                        chunk_hash: hash_array,
                        wal_position,
                    };

                    let mut new_file = InMemoryFile {
                        hasher: hasher,
                        chunks: vec![memory_chunk],
                    };

                    wmanifest.insert(file_uuid, new_file);
                    whash_index.insert(hash_array, file_uuid);
                }
                Err(e) => {
                    return Err(FileSystemError::LFSError { error: format!("wal append failed: {}", e)} );
                }
            }

            Payload {
                json: Some(serde_json::to_value(FsResponse::Write(hash_array)).unwrap()),
                bytes: None,
            }
        }
        FsAction::Read(file_hash) => {
            // obtain read locks.
            let rmanifest = manifest.read().await;
            let rhash_index = hash_index.read().await;
            let mut rlog = log.write().await;   // need mut for reading file, check

            let file_uuid = rhash_index.get(&file_hash)
                .ok_or_else(|| FileSystemError::LFSError { error: format!("no file found for hash: {:?}", file_hash) })?;
        
            let memfile = rmanifest.get(&file_uuid)
                .ok_or_else(|| FileSystemError::LFSError { error: format!("no file found for hash: {:?}", file_hash) })?;
        

            let mut data = Vec::new();
            for chunk in &memfile.chunks {
                // handle
                let bytes = get_chunk_data(&mut rlog, chunk.wal_position).await.unwrap();
                data.extend_from_slice(&bytes);
            }

            println!("read bytes: {:?}", data);

            Payload {
                json: Some(serde_json::to_value(FsResponse::Read(file_hash)).unwrap()),
                bytes: Some(data),
            }
        },
        FsAction::Append(file_hash) => {
            let Some(data) = content.payload.bytes.clone() else {
                return Err(FileSystemError::BadBytes { action: "Write".into() })
            };

            let file_uuid = {
                let rhash_index = hash_index.read().await;
                match rhash_index.get(&file_hash) {
                    Some(uuid) => *uuid,
                    None => return Err(FileSystemError::LFSError { error: format!("no file found for hash: {:?}", file_hash)} )
                }
            };
            
            // compute hash of the data chunk
            let mut hasher = blake3::Hasher::new();
            hasher.update(&data);
            let hash_result = hasher.finalize();
            let hash_array: [u8; 32] = *hash_result.as_bytes();
            
            // determine the new chunk range
            let previous_end_position: u64;
            {
                let rmanifest = manifest.read().await;
                match rmanifest.get(&file_uuid) {
                    Some(file) => previous_end_position = file.chunks.last().unwrap().chunk_range.1,
                    None => return Err(FileSystemError::LFSError { error: format!("no file found for hash: {:?}", file_hash)} )
                }
            }
            let chunk_range = (previous_end_position + 1, previous_end_position + data.len() as u64);

            let entry = ChunkEntry {
                file_uuid,
                chunk_range,
                chunk_hash: hash_array,
            };
            
            let wal_position;
            {
                let mut wlog = log.write().await;
                wal_position = append_to_wal(&mut wlog, &entry, &data).await.unwrap();
            }
            
            {
                let mut wmanifest = manifest.write().await;
                if let Some(memfile) = wmanifest.get_mut(&file_uuid) {
                    let memory_chunk = MemoryChunk {
                        chunk_range: entry.chunk_range,
                        chunk_hash: hash_array,
                        wal_position,
                    };
                    
                    memfile.hasher.update(&data);       // update file's hash with the new data chunk
                    memfile.chunks.push(memory_chunk); 
                } else {
                    return Err(FileSystemError::BadUri { uri: "".to_string(), bad_part_name: "".to_string(), bad_part:None });
                }
            }

            Payload {
                json: Some(serde_json::to_value(FsResponse::Append(file_hash)).unwrap()),
                bytes: None,
            }
        },
        FsAction::ReadChunk(req) => {
            // obtain read locks.            
            let rmanifest = manifest.read().await;
            let rhash_index = hash_index.read().await;
            let mut rlog = log.write().await;  // need mut for reading file, check
        
            // Find the file UUID from the file hash.
            let file_uuid = match rhash_index.get(&req.file_hash) {
                Some(uuid) => uuid,
                None => return Err(FileSystemError::LFSError { error: format!("no file found for hash: {:?}", req.file_hash)} )
            };
            
            // Get the memory file.
            let memfile = match rmanifest.get(&file_uuid) {
                Some(file) => file,
                None =>  return Err(FileSystemError::LFSError { error: format!("no file found for hash: {:?}", req.file_hash)} )
            };
            
            let mut data = Vec::new();
            
            for chunk in &memfile.chunks {
                if chunk.chunk_range.1 < req.start {
                    continue;  // chunk is entirely before the requested range
                }
        
                if chunk.chunk_range.0 > (req.start + req.length - 1) {
                    break;     // chunk is entirely after the requested range
                }
        
                let chunk_data = get_chunk_data(&mut rlog, chunk.wal_position).await.unwrap();
        
                // Handle overlap: Identify which part of the chunk should be taken.
                let chunk_start = chunk.chunk_range.0.max(req.start) - chunk.chunk_range.0;
                let chunk_end = chunk.chunk_range.1.min(req.start + req.length - 1) - chunk.chunk_range.0 + 1;
        
                data.extend_from_slice(&chunk_data[chunk_start as usize..chunk_end as usize]);
            }

            println!("read bytes: {:?}", data);
        
            Payload {
                json: Some(serde_json::to_value(FsResponse::ReadChunk(req.file_hash)).unwrap()),
                bytes: Some(data),
            }
        },
        // specific process manager write:
        FsAction::PmWrite => {
            let Some(data) = content.payload.bytes.clone() else {
                return Err(FileSystemError::BadBytes { action: "Write".into() })
            };

            //  unique process manager file. [hash not appendable]
            let file_uuid = pm_uuid;

            //  hashing: note chunks[]
            let mut hasher = blake3::Hasher::new();
            hasher.update(&data);
            let hash_result = hasher.finalize();
            let hash_array: [u8; 32] = *hash_result.as_bytes();

            let entry = ChunkEntry {
                file_uuid,
                chunk_range: (0, data.len() as u64 - 1),
                chunk_hash: hash_array.clone(),
            };

            let wal_result;
            {
                let mut wlog = log.write().await;
                wal_result = append_to_wal(&mut wlog, &entry, &data).await;
            }

            match wal_result {
                Ok(wal_position) => {
                    let mut wmanifest = manifest.write().await;
                    let mut whash_index = hash_index.write().await;

                    let memory_chunk = MemoryChunk {
                        chunk_range: entry.chunk_range,
                        chunk_hash: hash_array,
                        wal_position,
                    };

                    let mut new_file = InMemoryFile {
                        hasher: hasher,
                        chunks: vec![memory_chunk],
                    };

                    wmanifest.insert(file_uuid, new_file);
                    whash_index.insert(hash_array, file_uuid);
                }
                Err(e) => {
                    return Err(FileSystemError::LFSError { error: format!("wal append failed: {}", e)} );
                }
            }

            Payload {
                json: Some(serde_json::to_value(FsResponse::Write(hash_array)).unwrap()),
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

//  TODO: factor our with microkernel
fn get_current_unix_time() -> anyhow::Result<u64> {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(t) => Ok(t.as_secs()),
        Err(e) => Err(e.into()),
    }
}

async fn load_wal(
    log_file: &mut fs::File,
    manifest: &mut HashMap<u128, InMemoryFile>,
    hash_index: &mut HashMap<[u8; 32], u128>,
) -> Result<(), io::Error> {
    let mut current_position = 0;

    loop {
        // Seek to the current position
        log_file.seek(SeekFrom::Start(current_position)).await?;

        // Read length of the serialized metadata
        let mut length_buffer = [0u8; 8];
        let read_size: usize = log_file.read(&mut length_buffer).await?;

        if read_size < 8 {
            // Not enough data left to read metadata length, break out of the loop
            break;
        }
        let metadata_length = u64::from_le_bytes(length_buffer) as usize;

        // Read serialized metadata
        let mut metadata_buffer = vec![0u8; metadata_length];
        log_file.read_exact(&mut metadata_buffer).await?;
        let chunk_entry_metadata: Result<ChunkEntry, _> = bincode::deserialize(&metadata_buffer);

        // Based on metadata, read the data length and then the data
        let mut data_length_buffer = [0u8; 8];
        log_file.read_exact(&mut data_length_buffer).await?;

        let data_position = current_position + 8 + metadata_length as u64;

        let data_length = u64::from_le_bytes(data_length_buffer) as usize;

        //  note, we currently read in actual chunk data to hash, but this is avoidable, we have their length encoded.
        let mut data_buffer = vec![0u8; data_length];
        log_file.read_exact(&mut data_buffer).await?;

        match chunk_entry_metadata {
            Ok(mut entry) => {

                // Calculate the wal_position for the data
                let memory_chunk = MemoryChunk {
                    chunk_range: entry.chunk_range,
                    chunk_hash: entry.chunk_hash,
                    wal_position: data_position,
                };
                let is_pm_uuid = entry.file_uuid == pm_uuid;

                // Update the hasher with the data
                if let Some(memfile) = manifest.get_mut(&entry.file_uuid) {
                    // specific pm case, revise
                    if is_pm_uuid {
                        let mut new_file = InMemoryFile {
                            hasher: Hasher::new(),
                            chunks: vec![memory_chunk],
                        };
                        new_file.hasher.update(&data_buffer);
                        let file_hash = new_file.hasher.finalize();
                        let hash_array: [u8; 32] = *file_hash.as_bytes();
                
                        manifest.insert(entry.file_uuid, new_file);
                        hash_index.insert(hash_array, entry.file_uuid);
                    } else {
                        memfile.hasher.update(&data_buffer);
                        memfile.chunks.push(memory_chunk);
                    }
                
                } else {
                    let mut new_file = InMemoryFile {
                        hasher: Hasher::new(),
                        chunks: vec![memory_chunk],
                    };
                    new_file.hasher.update(&data_buffer);
                    let file_hash = new_file.hasher.finalize();
                    let hash_array: [u8; 32] = *file_hash.as_bytes();

                    manifest.insert(entry.file_uuid, new_file);
                    hash_index.insert(hash_array, entry.file_uuid);
                }

                // Move to the next position after the metadata, data length, and data
                current_position += (8 * 2) + metadata_length as u64 + data_length as u64;
            }
            Err(_) => {
                // If there's an error, break from the loop (might want to handle this better)
                break;
            }
        }
    }

    // Truncate the WAL file to the current position
    log_file.set_len(current_position).await?;
    Ok(())
}

async fn append_to_wal(
    log_file: &mut fs::File,
    entry: &ChunkEntry,
    data: &[u8],
) -> Result<u64, io::Error> {
    let wal_position = log_file.metadata().await?.len();

    let serialized_entry = bincode::serialize(&entry).unwrap();
    let entry_length = serialized_entry.len() as u64;
    let data_length = data.len() as u64;

    log_file.write_all(&entry_length.to_le_bytes()).await?;  // write the metadata length prefix
    log_file.write_all(&serialized_entry).await?;            // write the serialized metadata
    log_file.write_all(&data_length.to_le_bytes()).await?;   // write the data length
    log_file.write_all(data).await?;                         // write the data

    // return the location where the data starts in the WAL
    Ok(wal_position + (8 + serialized_entry.len() as u64))
}

async fn get_chunk_data(log_file: &mut fs::File, wal_position: u64) -> Result<Vec<u8>, io::Error> {
    // Seek to the provided position in the WAL
    log_file.seek(SeekFrom::Start(wal_position)).await?;

    // Read the length of the data
    let mut length_buffer = [0u8; 8];
    log_file.read_exact(&mut length_buffer).await?;
    let data_length = u64::from_le_bytes(length_buffer) as usize;

    // Read the data
    let mut data_buffer = vec![0u8; data_length];
    log_file.read_exact(&mut data_buffer).await?;

    Ok(data_buffer)
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
            timestamp: get_current_unix_time().unwrap(),       //  TODO: handle error?
            content: UqbarErrorContent {
                kind: error.kind().into(),
                // message: format!("{}", error),
                message: serde_json::to_value(error).unwrap(), //  TODO: handle error?
                context: serde_json::to_value("").unwrap(),
            },
        }),
    }
}

pub async fn pm_bootstrap(
    home_directory_path: String,
) -> Result<Option<(Vec<u8>, [u8; 32])>, FileSystemError> {  

    // fs bootstrapping, create home_directory and log file if none.
    create_dir_if_dne(&home_directory_path).await
        .map_err(|_| FileSystemError::LFSError { error: "couldn't create home dir".into() })?;
    
    let home_directory_path = fs::canonicalize(home_directory_path).await
        .map_err(|_| FileSystemError::LFSError { error: "couldn't canonicalize path".into() })?;

    //  open log file, load it in.
    let log_file_path = home_directory_path.join("log.bin");
    let mut log_file = fs::OpenOptions::new()
        .append(true)
        .read(true)
        .create(true)
        .open(&log_file_path)
        .await
        .map_err(|_| FileSystemError::LFSError { error: "failed to open log file".into() })?;

    //  in memory details about files.
    let mut manifest: HashMap<u128, InMemoryFile> = HashMap::new();
    //  enable lookup by file_hash
    let mut hash_index: HashMap<[u8; 32], u128> = HashMap::new();

    // Loading the WAL
    load_wal(&mut log_file, &mut manifest, &mut hash_index)
        .await
        .map_err(|_| FileSystemError::LFSError { error: "wal loading failed".into() })?;

    // Check for the pm_uuid entry in the manifest
    if let Some(pm_file) = manifest.get(&pm_uuid) {
        // If found, prepare the data to return
        let mut data = Vec::new();
        for chunk in &pm_file.chunks {
            // Assuming you have a get_chunk_data function to fetch the chunk's data
            let bytes = get_chunk_data(&mut log_file, chunk.wal_position).await
                .map_err(|_| FileSystemError::LFSError { error: "error getting chunk data".into() })?;
            data.extend_from_slice(&bytes);
        }
        
        // Fetch the filehash for the pm_uuid
        let file_hash = pm_file.hasher.finalize();
        let hash_array: [u8; 32] = *file_hash.as_bytes();

        // Return the data and filehash
        return Ok(Some((data, hash_array)));
    }

    // If pm_uuid entry not found
    Ok(None)
}