///  append-only log file with commited changes before they're applied to disk
/// 
use serde::{Deserialize, Serialize};



//   ON-DISK, WAL
#[derive(Serialize, Deserialize)]
enum WALRecord {
    Chunk(ChunkEntry), // wrote bytes to wal
}

#[derive(Serialize, Deserialize, Clone)]
struct ChunkEntry {
    file_uuid: u128,
    chunk_range: (u64, u64),
    chunk_hash: [u8; 32],
    //  data: Vec<u8>, data is kept separately after the chunk_entry
}

// IN-MEMORY, WAL
#[derive(Debug, Clone)]
struct InMemoryChunks {
    _chunks: Vec<MemoryChunk>,
    _partial: Option<PartialChunk>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MemoryChunk {
    chunk_range: (u64, u64), // start and end positions in the file
    chunk_hash: [u8; 32],    // data hash by itself.
    wal_position: u64,       // position of this chunk's data in the WAL.
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PartialChunk {
    chunk_range: (u64, u64), // start and end positions in the file
    chunk_hash: [u8; 32],    // data hash by itself.
    wal_position: u64,       // position of this chunk's data in the WAL.
    data: Vec<u8>,
}

#[derive(Serialize, Deserialize, Clone)]
struct BackupMutable {
    file_uuid: u128,
    file_hash: [u8; 32],
    file_length: u64,
    local: bool,
    immutable: bool,
    backup: Vec<[u8; 32]>,
    chunk_hashes: Vec<MemoryChunk>,
    partial_chunk: Option<PartialChunk>,
}


async fn _append_to_wal(
    log_file: &mut fs::File,
    entry: &ChunkEntry,
    data: &[u8],
) -> Result<u64, io::Error> {
    let wal_position = log_file.metadata().await?.len();

    let serialized_entry = bincode::serialize(&entry).unwrap();
    let entry_length = serialized_entry.len() as u64;
    let data_length = data.len() as u64;

    log_file.write_all(&entry_length.to_le_bytes()).await?; // write the metadata length prefix
    log_file.write_all(&serialized_entry).await?;           // write the serialized metadata
    log_file.write_all(&data_length.to_le_bytes()).await?;  // write the data length
    log_file.write_all(data).await?;                        // write the data

    // return the location where the data starts in the WAL
    Ok(wal_position + (8 + serialized_entry.len() as u64))
}

async fn _get_chunk_data(log_file: &mut fs::File, wal_position: u64) -> Result<Vec<u8>, io::Error> {
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