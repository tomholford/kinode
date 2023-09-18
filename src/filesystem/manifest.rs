use crate::types::{FileSystemError, ProcessId};
use blake3::Hasher;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;
use tokio::io::{self, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::sync::RwLock;
use uuid;

/// Contains interface for filesystem manifest log, and write ahead log.

//   ON-DISK, WAL
#[derive(Serialize, Deserialize)]
pub enum WALRecord {
    Chunk(ChunkEntry),
    SetLength(FileIdentifier, u64),
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ChunkEntry {
    file: FileIdentifier,
    start: u64,
    length: u64,
    chunk_hash: [u8; 32],
    copy: bool,
    //  data: Vec<u8>, kept separately after the chunk_entry bytes
}

// this one is a bit of an experiment. we need some unqiueness source for files.
// don't see a reason to separate file_uuid and process_id out completely.
#[derive(Debug, Serialize, Deserialize, Clone, Eq, Hash, PartialEq)]
pub enum FileIdentifier {
    UUID(u128),
    Process(ProcessId),
}

//   ON-DISK, MANIFEST
#[derive(Serialize, Deserialize, Clone)]
pub enum ManifestRecord {
    Backup(BackupEntry),
    Delete(FileIdentifier),
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BackupEntry {
    pub file: FileIdentifier,
    pub chunks: Vec<([u8; 32], u64, u64)>, // (hash, start, length)
}

// IN-MEMORY, MANIFEST
#[derive(Debug, Clone)]
pub struct InMemoryFile {
    pub chunks: BTreeMap<u64, ([u8; 32], u64, Option<u64>)>, // (start) (hash, length, option<wal_location>)
}

impl InMemoryFile {
    pub fn hash(&self) -> [u8; 32] {
        let mut hasher = Hasher::new();
        for (_, (hash, _, _)) in &self.chunks {
            hasher.update(hash);
        }
        hasher.finalize().into()
    }

    pub fn find_chunks_in_range(
        &self,
        start: u64,
        length: u64,
    ) -> Vec<(u64, ([u8; 32], u64, Option<u64>))> {
        let end = start + length;
        let adjusted_start = start.saturating_sub(super::CHUNK_SIZE as u64);
        let adjusted_end = end.saturating_add(super::CHUNK_SIZE as u64);

        self.chunks
            .range(adjusted_start..adjusted_end)
            .filter(|&(chunk_start, (_, chunk_length, _))| {
                let chunk_end = chunk_start + chunk_length;
                chunk_start < &end && chunk_end > start
            })
            .map(|(&start, value)| (start, *value))
            .collect()
    }

    pub fn get_len(&self) -> u64 {
        self.chunks
            .iter()
            .last()
            .map_or(0, |(&start, (_, length, _))| start + length)
    }

    pub fn _get_last_chunk(&self) -> Option<(u64, ([u8; 32], u64, Option<u64>))> {
        self.chunks
            .iter()
            .last()
            .map(|(&start, value)| (start, *value))
    }

    // write_chunk helper.
}

impl Default for InMemoryFile {
    fn default() -> Self {
        Self {
            chunks: BTreeMap::new(),
        }
    }
}

impl FileIdentifier {
    pub fn new_uuid() -> Self {
        Self::UUID(uuid::Uuid::new_v4().as_u128())
    }

    pub fn to_uuid(&self) -> Option<u128> {
        match self {
            Self::UUID(uuid) => Some(*uuid),
            _ => None,
        }
    }
}
#[derive(Debug)]
pub struct Manifest {
    pub manifest: Arc<RwLock<HashMap<FileIdentifier, InMemoryFile>>>,
    pub chunk_hashes: Arc<RwLock<HashSet<[u8; 32]>>>,
    pub hash_index: Arc<RwLock<HashMap<[u8; 32], FileIdentifier>>>,
    pub manifest_file: Arc<RwLock<fs::File>>,
    pub wal_file: Arc<RwLock<fs::File>>,
    pub fs_directory_path: PathBuf,
}

impl Manifest {
    pub async fn load(
        manifest_file: fs::File,
        wal_file: fs::File,
        fs_directory_path: &PathBuf,
    ) -> io::Result<Self> {
        let mut manifest = HashMap::new();
        let mut chunk_hashes = HashSet::new();
        let mut hash_index = HashMap::new();
        let mut manifest_file = manifest_file;
        let mut wal_file = wal_file;

        load_manifest(&mut manifest_file, &mut manifest).await?;
        load_wal(&mut wal_file, &mut manifest).await?;

        verify_manifest(&mut manifest, &mut chunk_hashes, &mut hash_index).await?;

        Ok(Self {
            manifest: Arc::new(RwLock::new(manifest)),
            chunk_hashes: Arc::new(RwLock::new(chunk_hashes)),
            hash_index: Arc::new(RwLock::new(hash_index)),
            manifest_file: Arc::new(RwLock::new(manifest_file)),
            wal_file: Arc::new(RwLock::new(wal_file)),
            fs_directory_path: fs_directory_path.clone(),
        })
    }

    pub async fn get(&self, file: &FileIdentifier) -> Option<InMemoryFile> {
        let read_lock = self.manifest.read().await;
        read_lock.get(&file).cloned()
    }

    pub async fn get_length(&self, file: &FileIdentifier) -> Option<u64> {
        let read_lock = self.manifest.read().await;
        read_lock.get(&file).map(|f| f.get_len())
    }

    pub async fn get_by_hash(&self, hash: &[u8; 32]) -> Option<FileIdentifier> {
        let read_lock = self.hash_index.read().await;
        read_lock.get(hash).cloned()
    }

    pub async fn get_chunk_hashes(&self) -> HashSet<[u8; 32]> {
        let mut in_use_hashes = HashSet::new();
        for file in self.manifest.read().await.values() {
            for (_start, (hash, _length, _wal_position)) in &file.chunks {
                in_use_hashes.insert(*hash);
            }
        }
        in_use_hashes
    }

    pub async fn _get_file_hashes(&self) -> HashMap<FileIdentifier, [u8; 32]> {
        let mut file_hashes = HashMap::new();
        for (file_id, file) in self.manifest.read().await.iter() {
            file_hashes.insert(file_id.clone(), file.hash());
        }
        file_hashes
    }

    pub async fn get_uuid_by_hash(&self, hash: &[u8; 32]) -> Option<u128> {
        let read_lock = self.hash_index.read().await;
        if let Some(file_id) = read_lock.get(hash) {
            file_id.to_uuid()
        } else {
            None
        }
    }

    pub async fn write(&self, file: &FileIdentifier, data: &[u8]) -> Result<(), FileSystemError> {
        let mut manifest = self.manifest.write().await;
        let mut in_memory_file = InMemoryFile::default();

        let mut chunks = data.chunks(super::CHUNK_SIZE);
        let mut chunk_start = 0u64;

        while let Some(chunk) = chunks.next() {
            self.write_chunk(file, chunk, chunk_start, &mut in_memory_file)
                .await?;

            chunk_start += chunk.len() as u64;
        }

        manifest.insert(file.clone(), in_memory_file);

        Ok(())
    }

    pub async fn write_chunk(
        &self,
        file: &FileIdentifier,
        chunk: &[u8],
        start: u64,
        in_memory_file: &mut InMemoryFile,
    ) -> Result<(), FileSystemError> {
        let mut wal_file = self.wal_file.write().await;
        let chunk_hashes = self.chunk_hashes.read().await;

        let chunk_hash: [u8; 32] = blake3::hash(&chunk).into();
        let chunk_length = chunk.len() as u64;
        let copy = chunk_hashes.contains(&chunk_hash);

        let entry = ChunkEntry {
            file: file.clone(),
            start,
            length: chunk_length,
            chunk_hash,
            copy,
        };

        // serialize the metadata
        let serialized_metadata = bincode::serialize(&WALRecord::Chunk(entry)).unwrap();

        // write the chunk data to the WAL file, fsync?
        let metadata_length = serialized_metadata.len() as u64;
        wal_file.write_all(&metadata_length.to_le_bytes()).await?;

        wal_file.write_all(&serialized_metadata).await?;
        if !copy {
            wal_file.write_all(chunk).await?;
        }

        // calculate the position for the chunk in memory chunks
        let position = wal_file.stream_position().await?;
        let proper_position = if copy {
            None
        } else {
            Some(position - chunk.len() as u64)
        };

        // update the in_memory_file directly
        in_memory_file
            .chunks
            .insert(start, (chunk_hash, chunk_length, proper_position));

        Ok(())
    }

    pub async fn read(
        &self,
        file_id: &FileIdentifier,
        start: Option<u64>,
        length: Option<u64>,
    ) -> Result<Vec<u8>, FileSystemError> {
        let file = self.get(file_id).await.ok_or(FileSystemError::LFSError {
            error: "File not found in manifest".to_string(),
        })?;

        let mut data = Vec::new();

        // filter chunks based on start and length if they are defined
        let filtered_chunks = if let (Some(start), Some(length)) = (start, length) {
            file.find_chunks_in_range(start, length)
        } else {
            // doublecheck if bad!
            file.chunks
                .iter()
                .map(|(&start, &value)| (start, value))
                .collect()
        };

        for (start_chunk, (hash, len, wal_position)) in filtered_chunks {
            let mut chunk_data = if let Some(wal_position) = wal_position {
                let mut wal_file = self.wal_file.write().await;
                wal_file
                    .seek(SeekFrom::Start(wal_position))
                    .await
                    .map_err(|e| FileSystemError::LFSError {
                        error: format!("Failed to seek in WAL file: {}", e),
                    })?;
                let mut buffer = vec![0u8; len as usize];
                wal_file
                    .read_exact(&mut buffer)
                    .await
                    .map_err(|e| FileSystemError::LFSError {
                        error: format!("Failed to read from WAL file: {}", e),
                    })?;
                buffer
            } else {
                let path = self.fs_directory_path.join(hex::encode(hash));
                fs::read(path)
                    .await
                    .map_err(|e| FileSystemError::LFSError {
                        error: format!("Failed to read from file system: {}", e),
                    })?
            };

            //  doublecheck
            if let Some(start) = start {
                if start > start_chunk {
                    chunk_data.drain(..(start - start_chunk) as usize);
                }
            }
            if let Some(length) = length {
                let end = start.unwrap_or(0) + length;
                if end < start_chunk + len {
                    chunk_data.truncate((end - start_chunk) as usize);
                }
            }

            data.append(&mut chunk_data);
        }

        Ok(data)
    }

    pub async fn write_at(
        &self,
        file_id: &FileIdentifier,
        offset: u64,
        data: &[u8],
    ) -> Result<(), FileSystemError> {
        let mut file = self.get(file_id).await.ok_or(FileSystemError::LFSError {
            error: "File not found in manifest".to_string(),
        })?;

        let affected_chunks = file.find_chunks_in_range(offset, data.len() as u64);
        let mut data_offset = 0;

        for (start, (_hash, length, _wal_position)) in affected_chunks {
            let chunk_data_start = if start < offset {
                (offset - start) as usize
            } else {
                0
            };
            let remaining_length = length as usize - chunk_data_start;
            let remaining_data = data.len() - data_offset;
            let write_length = remaining_length.min(remaining_data);

            let mut chunk_data = self.read(file_id, Some(start), Some(length)).await?;
            chunk_data.resize(chunk_data_start + write_length, 0); // extend the chunk data if necessary

            let data_to_write = &data[data_offset..data_offset + write_length as usize];
            chunk_data[chunk_data_start..chunk_data_start + write_length as usize]
                .copy_from_slice(data_to_write);

            self.write_chunk(file_id, &chunk_data, start, &mut file)
                .await?;
            data_offset += write_length;
        }

        // if there's still data left to write, create a new chunk
        if data_offset < data.len() {
            let remaining_data = &data[data_offset..];
            let start = file.get_len();
            self.write_chunk(file_id, remaining_data, start, &mut file)
                .await?;
        }

        let mut manifest = self.manifest.write().await;
        manifest.insert(file_id.clone(), file);

        Ok(())
    }

    pub async fn append(
        &self,
        file_id: &FileIdentifier,
        data: &[u8],
    ) -> Result<(), FileSystemError> {
        let file = self.get(file_id).await.ok_or(FileSystemError::LFSError {
            error: "File not found in manifest".to_string(),
        })?;

        let offset = file.get_len();
        self.write_at(file_id, offset, data).await
    }

    pub async fn set_length(
        &self,
        file_id: &FileIdentifier,
        new_length: u64,
    ) -> Result<(), FileSystemError> {
        let mut file = self.get(file_id).await.ok_or(FileSystemError::LFSError {
            error: "File not found in manifest".to_string(),
        })?;

        let file_len = file.get_len();

        if new_length > file_len {
            // extend with zeroes
            let extension_length = new_length - file_len;
            let extension_data = vec![0; extension_length as usize];
            self.write_chunk(file_id, &extension_data, file_len, &mut file)
                .await?;
        } else if new_length < file_len {
            // truncate
            let affected_chunk = file.find_chunks_in_range(new_length, 1);
            if let Some((start, (_hash, length, _wal_position))) = affected_chunk.first() {
                let mut chunk_data = self.read(file_id, Some(*start), Some(*length)).await?;
                chunk_data.truncate((new_length - start) as usize);
                self.write_chunk(file_id, &chunk_data, *start, &mut file)
                    .await?;
            }
            file.chunks.retain(|&start, _| start < new_length);
        }

        // set_length entry to wal, will do the same file.chunks.retain upon reload/flush
        let entry = WALRecord::SetLength(file_id.clone(), new_length);
        let serialized_entry = bincode::serialize(&entry).unwrap();
        let entry_length = serialized_entry.len() as u64;

        let mut wal_file = self.wal_file.write().await;

        wal_file.write_all(&entry_length.to_le_bytes()).await?;
        wal_file.write_all(&serialized_entry).await?;

        let mut manifest = self.manifest.write().await;
        manifest.insert(file_id.clone(), file);

        Ok(())
    }

    pub async fn flush(&self) -> Result<(), FileSystemError> {
        let mut manifest_lock = self.manifest.write().await;
        let mut wal_file = self.wal_file.write().await;
        let mut manifest_file = self.manifest_file.write().await;
        let mut chunk_hashes = self.chunk_hashes.write().await;
        let mut hash_index = self.hash_index.write().await;

        for (file_id, in_memory_file) in manifest_lock.iter_mut() {
            let chunks_to_flush: Vec<([u8; 32], u64, u64, u64)> = in_memory_file
                .chunks
                .iter()
                .filter_map(|(&start, &(hash, length, wal_position))| {
                    wal_position.map(|wal_pos| (hash, start, length, wal_pos))
                })
                .collect();

            for (hash, start, length, wal_position) in chunks_to_flush.iter() {
                // seek to the chunk in the WAL file
                wal_file.seek(SeekFrom::Start(*wal_position)).await?;

                // read the chunk data from the WAL file
                let mut buffer = vec![0u8; *length as usize];
                wal_file.read_exact(&mut buffer).await?;

                // write the chunk data to a new file in the filesystem
                let path = self.fs_directory_path.join(hex::encode(hash));
                fs::write(path, &buffer).await?;

                // add a manifest entry with the new hash and removed wal_position
                in_memory_file.chunks.insert(*start, (*hash, *length, None));

                chunk_hashes.insert(*hash);
            }

            if !chunks_to_flush.is_empty() {
                // add updated manifest entries to the manifest file
                let entry = ManifestRecord::Backup(BackupEntry {
                    file: file_id.clone(),
                    chunks: in_memory_file
                        .chunks
                        .iter()
                        .map(|(&k, &v)| (v.0, k, v.1))
                        .collect::<Vec<_>>(),
                });
                let serialized_entry = bincode::serialize(&entry).unwrap();
                let entry_length = serialized_entry.len() as u64;

                let mut buffer = Vec::new();
                buffer.extend_from_slice(&entry_length.to_le_bytes());
                buffer.extend_from_slice(&serialized_entry);

                manifest_file.write_all(&buffer).await?;
                hash_index.insert(in_memory_file.hash(), file_id.clone());
            }
        }

        // clear the WAL file
        wal_file.set_len(0).await?;

        Ok(())
    }

    pub async fn delete(&self, file_id: &FileIdentifier) -> Result<(), FileSystemError> {
        // add a delete entry to the manifest
        let entry = ManifestRecord::Delete(file_id.clone());
        let serialized_entry = bincode::serialize(&entry).unwrap();
        let entry_length = serialized_entry.len() as u64;
        let mut manifest_file = self.manifest_file.write().await;

        manifest_file.write_all(&entry_length.to_le_bytes()).await?;
        manifest_file.write_all(&serialized_entry).await?;
        // manifest_file.sync_all().await?;

        // Remove the file from the manifest
        let mut manifest = self.manifest.write().await;
        manifest.remove(file_id);

        Ok(())
    }

    pub async fn cleanup(&self) -> Result<(), FileSystemError> {
        let in_use_hashes = self.get_chunk_hashes().await;

        // loop through all chunks on disk
        let mut entries = fs::read_dir(&self.fs_directory_path).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let file_name = path.file_name().and_then(|os_str| os_str.to_str());

            if let Some(file_name) = file_name {
                if file_name == "manifest.bin" || file_name == "wal.bin" {
                    continue;
                }

                if let Ok(vec) = hex::decode(file_name) {
                    let hash: [u8; 32] = match vec[..].try_into() {
                        Ok(array) => array,
                        Err(_) => continue, // jf the conversion fails, skip
                    };

                    // if the chunk is used, delete it
                    if !in_use_hashes.contains(&hash) {
                        let _ = fs::remove_file(path).await;
                    }
                }
            }
        }

        Ok(())
    }
}

impl Clone for Manifest {
    fn clone(&self) -> Self {
        Self {
            manifest: Arc::clone(&self.manifest),
            chunk_hashes: Arc::clone(&self.chunk_hashes),
            hash_index: Arc::clone(&self.hash_index),
            manifest_file: Arc::clone(&self.manifest_file),
            wal_file: Arc::clone(&self.wal_file),
            fs_directory_path: self.fs_directory_path.clone(),
        }
    }
}

async fn load_manifest(
    manifest_file: &mut fs::File,
    manifest: &mut HashMap<FileIdentifier, InMemoryFile>,
) -> Result<(), io::Error> {
    let mut current_position = 0;

    loop {
        // Seek to the current position
        manifest_file
            .seek(SeekFrom::Start(current_position))
            .await?;

        // Read length of the serialized metadata
        let mut length_buffer = [0u8; 8];
        let read_size: usize = manifest_file.read(&mut length_buffer).await?;

        if read_size < 8 {
            // Not enough data left to read metadata length, break out of the loop
            break;
        }
        let metadata_length = u64::from_le_bytes(length_buffer) as usize;

        // Read serialized metadata
        let mut metadata_buffer = vec![0u8; metadata_length];
        manifest_file.read_exact(&mut metadata_buffer).await?;
        let record_metadata: Result<ManifestRecord, _> = bincode::deserialize(&metadata_buffer);

        match record_metadata {
            Ok(ManifestRecord::Backup(entry)) => {
                manifest.insert(
                    entry.file,
                    InMemoryFile {
                        chunks: entry
                            .chunks
                            .iter()
                            .map(|(hash, start, length)| (*start, (*hash, *length, None)))
                            .collect(),
                    },
                );
                // move to the next position after the metadata,
                current_position += 8 + metadata_length as u64;
            }
            Ok(ManifestRecord::Delete(delete)) => {
                manifest.remove(&delete);
                current_position += 8 + metadata_length as u64;
            }

            Err(_) => {
                // faulty entry, remove.
                break;
            }
        }
    }
    // truncate the manifest file to the current position
    manifest_file.set_len(current_position).await?;
    Ok(())
}

async fn load_wal(
    wal_file: &mut fs::File,
    manifest: &mut HashMap<FileIdentifier, InMemoryFile>,
) -> Result<(), io::Error> {
    let mut current_position = 0;

    loop {
        // seek to the current position
        wal_file.seek(SeekFrom::Start(current_position)).await?;

        // read length of the serialized metadata
        let mut length_buffer = [0u8; 8];
        let read_size: usize = wal_file.read(&mut length_buffer).await?;

        if read_size < 8 {
            // not enough data left to read metadata length, break out of the loop
            break;
        }
        let record_length = u64::from_le_bytes(length_buffer) as usize;

        // read serialized metadata
        let mut record_buffer = vec![0u8; record_length];
        match wal_file.read_exact(&mut record_buffer).await {
            Ok(_) => {
                let record: Result<WALRecord, _> = bincode::deserialize(&record_buffer);
                match record {
                    Ok(WALRecord::Chunk(entry)) => {
                        let data_position: u64 = current_position + 8 + record_length as u64;
                        let data_length = entry.length;
                        //  let end = entry.start + entry.length;
                        //  todo: examine chance from corruption that (range, range) length doesn't match data buffer length.

                        let in_memory_file = manifest
                            .entry(entry.file)
                            .or_insert(InMemoryFile::default());

                        let wal_position = if entry.copy {
                            None
                        } else {
                            Some(data_position)
                        };
                        in_memory_file
                            .chunks
                            .insert(entry.start, (entry.chunk_hash, entry.length, wal_position));

                        // if it's a copy, we don't have to skip + data_length to get to the next position
                        current_position += 8 + record_length as u64;
                        if !entry.copy {
                            current_position += data_length as u64;
                        }
                    }
                    Ok(WALRecord::SetLength(file_id, new_length)) => {
                        let in_memory_file =
                            manifest.entry(file_id).or_insert(InMemoryFile::default());
                        in_memory_file.chunks.retain(|&start, _| start < new_length);
                        current_position += 8 + record_length as u64;
                    }
                    Err(_) => {
                        //  println!("failed to deserialize WALRecord.");
                        break;
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                //  println!("Encountered an incomplete record. Truncating the file.");
                break;
            }
            Err(e) => return Err(e),
        }
    }

    // truncate the WAL file to the current position
    wal_file.set_len(current_position).await?;
    Ok(())
}

async fn verify_manifest(
    manifest: &mut HashMap<FileIdentifier, InMemoryFile>,
    chunk_hashes: &mut HashSet<[u8; 32]>,
    hash_index: &mut HashMap<[u8; 32], FileIdentifier>,
) -> tokio::io::Result<()> {
    for (file, in_memory_file) in manifest.iter_mut() {
        let file_hash = in_memory_file.hash();
        //  note, do we want to add all chunks here?
        for (chunk_hash, _, wal_position) in in_memory_file.chunks.values() {
            if wal_position.is_none() {
                chunk_hashes.insert(*chunk_hash);
            }
        }
        hash_index.insert(file_hash, file.clone());
    }
    Ok(())
}

impl From<std::io::Error> for FileSystemError {
    fn from(error: std::io::Error) -> Self {
        FileSystemError::LFSError {
            error: format!("IO error: {}", error),
        }
    }
}
