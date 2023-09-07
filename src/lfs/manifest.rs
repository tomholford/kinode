use blake3::Hasher;
///  append-only log file of your local filesystem (and backups)
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;
use tokio::io::{self, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::sync::RwLock;
use super::wal::ToDelete;
use crate::types::{FileSystemError, ProcessId};

//   ON-DISK, MANIFEST
#[derive(Serialize, Deserialize, Clone)]
pub enum ManifestRecord {
    Backup(BackupEntry),
    Delete(u128),
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BackupEntry {
    pub file_uuid: u128,
    pub file_hash: [u8; 32],
    pub file_type: FileType,
    pub process: Option<ProcessId>,
    pub file_length: u64,
    pub local: bool,
    pub backup: Vec<[u8; 32]>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Copy)]
pub enum FileType {
    Appendable,
    Immutable,
    Writeable,
}

// IN-MEMORY, MANIFEST

#[derive(Debug, Clone)]
pub struct InMemoryFile {
    pub hash: [u8; 32], // content addressed hash (naive)
    pub file_type: FileType,
    pub hasher: Hasher,
    pub file_length: u64,
}

pub struct Manifest {
    pub manifest: Arc<RwLock<HashMap<u128, InMemoryFile>>>,
    pub process_state: Arc<RwLock<HashMap<ProcessId, InMemoryFile>>>,
    pub hash_index: Arc<RwLock<HashMap<[u8; 32], u128>>>,
    pub manifest_file: Arc<RwLock<fs::File>>,
    pub fs_directory_path: PathBuf,
}

impl Manifest {
    pub async fn load(manifest_file: fs::File, lfs_directory_path: &PathBuf) -> io::Result<Self> {
        let mut manifest = HashMap::new();
        let mut process_state = HashMap::new();
        let mut hash_index = HashMap::new();
        let mut manifest_file = manifest_file;

        load_manifest(&mut manifest_file, &mut manifest, &mut process_state).await?;

        verify_manifest(&mut manifest, &lfs_directory_path, &mut hash_index).await?;

        Ok(Self {
            manifest: Arc::new(RwLock::new(manifest)),
            process_state: Arc::new(RwLock::new(process_state)),
            hash_index: Arc::new(RwLock::new(hash_index)),
            manifest_file: Arc::new(RwLock::new(manifest_file)),
            fs_directory_path: lfs_directory_path.clone(),
        })
    }

    pub async fn get_by_uuid(&self, uuid: &u128) -> Option<InMemoryFile> {
        let read_lock = self.manifest.read().await;
        read_lock.get(uuid).cloned()
    }

    pub async fn get_by_hash(&self, file_hash: &[u8; 32]) -> Option<(InMemoryFile, u128)> {
        let read_lock = self.hash_index.read().await;
        if let Some(uuid) = read_lock.get(file_hash) {
            let manifest_lock = self.manifest.read().await;
            if let Some(file) = manifest_lock.get(uuid) {
                return Some((file.clone(), *uuid));
            }
        }
        None
    }

    pub async fn get_by_process(&self, process: &ProcessId) -> Option<InMemoryFile> {
        let read_lock = self.process_state.read().await;
        read_lock.get(process).cloned()
    }

    pub async fn get_process_state_hashes(&self) -> Vec<[u8; 32]> {
        let read_lock = self.process_state.read().await;
        read_lock.values().map(|file| file.hash).collect()
    }

    pub async fn add_local(
        &self,
        entry: &BackupEntry,
    ) -> Result<(), FileSystemError> {
        let mut manifest_file = self.manifest_file.write().await;

        let record = ManifestRecord::Backup(entry.clone());
        let serialized_entry = bincode::serialize(&record).unwrap();
        let entry_length = serialized_entry.len() as u64;

        let mut buffer = Vec::new();
        buffer.extend_from_slice(&entry_length.to_le_bytes()); // add the metadata length prefix
        buffer.extend_from_slice(&serialized_entry);           // add the serialized metadata

        if let Err(e) = manifest_file.write_all(&buffer).await {
            return Err(FileSystemError::LFSError {
                error: format!("write failed: {}", e),
            });
        }

        let mut wmanifest = self.manifest.write().await;
        let mut whash_index = self.hash_index.write().await;

        if let Some(process) = &entry.process {
            let mut wprocess_state = self.process_state.write().await;
            wprocess_state.insert(process.clone(),
                InMemoryFile {
                    hash: entry.file_hash,
                    file_type: entry.file_type,
                    hasher: Hasher::new(),
                    file_length: entry.file_length,
                },
            );
        } else {
            wmanifest.insert(
                entry.file_uuid,
                InMemoryFile {
                    hash: entry.file_hash,
                    file_type: entry.file_type,
                    hasher: Hasher::new(),
                    file_length: entry.file_length,
                },
            );
            whash_index.insert(entry.file_hash, entry.file_uuid);
        }
        Ok(())
    }

    pub async fn read(&self, file_uuid: u128, start: Option<u64>, length: Option<u64>) -> Result<Vec<u8>, FileSystemError> {
        let file = self.get_by_uuid(&file_uuid).await.ok_or_else(|| FileSystemError::LFSError {
            error: format!("no file found for id: {:?}", file_uuid),
        })?;

        let data = match file.file_type {
            FileType::Appendable => {
                super::read_mutable(file_uuid, &self.fs_directory_path, start, length).await?
            }
            FileType::Immutable => {
                super::read_immutable(&file.hash, &self.fs_directory_path, start, length).await?
            }
            FileType::Writeable => {
                super::read_mutable(file_uuid, &self.fs_directory_path, start, length).await?
            }
        };
        Ok(data)
    }

    pub async fn cleanup(&self) -> io::Result<()> {
        // temp disable for safety.
        //  return Ok(());
        // todo lock everything during this.

        let mut entries = fs::read_dir(&self.fs_directory_path).await?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let file_name = path.file_name().unwrap().to_str().unwrap();

            if file_name == "manifest.bin" || file_name == "wal.bin" {
                continue;
            }

            let process_hashes = self.get_process_state_hashes().await;
            // let process_hashes_hex: Vec<String> = process_hashes.iter().map(|hash| hex::encode(hash)).collect();
            let hash_index = self.hash_index.read().await;
            // let hash_index_hex: Vec<String> = hash_index.keys().map(|hash| hex::encode(hash)).collect();

            if let Ok(uuid) = file_name.parse::<u128>() {
                // additional checks
                if self.get_by_uuid(&uuid).await.is_none() {
                    let _ = fs::remove_file(path).await;
                }
            } else if let Ok(file_hash) = hex::decode(file_name) {
                let mut hash = [0u8; 32];
                hash.copy_from_slice(&file_hash);


                if self.get_by_hash(&hash).await.is_none() {
                    if !process_hashes.contains(&hash) {
                        let _ = fs::remove_file(path).await;
                    }
                }
            }

        }
        Ok(())
    }


    pub async fn add_delete(&self, file: &ToDelete) -> Result<(), FileSystemError> {
        let file_uuid = match file {
            ToDelete::Mutable(uuid) => *uuid,
            ToDelete::Immutable(hash) => {
                if let Some((_, uuid)) = self.get_by_hash(hash).await {
                    uuid
                } else {
                    return Err(FileSystemError::LFSError {
                        error: "File hash not found".to_string(),
                    });
                }
            }
        };

        if file_uuid == 0 {
            return Ok(());  // pmwrite case
        }

        let entry = ManifestRecord::Delete(file_uuid.clone());

        // serialize and write to on-disk manifest
        let mut manifest_file = self.manifest_file.write().await;
        let serialized_entry = bincode::serialize(&entry).unwrap();
        let entry_length = serialized_entry.len() as u64;

        let mut buffer = Vec::new();
        buffer.extend_from_slice(&entry_length.to_le_bytes());
        buffer.extend_from_slice(&serialized_entry);

        if let Err(e) = manifest_file.write_all(&buffer).await {
            return Err(FileSystemError::LFSError {
                error: format!("write failed: {}", e),
            });
        }

        // remove from in-memory manifest
        let mut wmanifest = self.manifest.write().await;
        if let Some(file) = wmanifest.remove(&file_uuid) {
            let mut whash_index = self.hash_index.write().await;
            whash_index.remove(&file.hash);
        }

        Ok(())
    }

    // note: in-mem only
    pub async fn insert(&self, file_uuid: u128, new_file: InMemoryFile) -> Result<(), FileSystemError> {
        let mut wmanifest = self.manifest.write().await;
        let mut whash_index = self.hash_index.write().await;

        // remove old hash index entry if it exists
        if let Some(old_file) = wmanifest.get(&file_uuid) {
            whash_index.remove(&old_file.hash);
        }

        wmanifest.insert(file_uuid, new_file.clone());
        whash_index.insert(new_file.hash, file_uuid);

        Ok(())
    }
}

impl Clone for Manifest {
    fn clone(&self) -> Self {
        Self {
            manifest: Arc::clone(&self.manifest),
            process_state: Arc::clone(&self.process_state),
            hash_index: Arc::clone(&self.hash_index),
            manifest_file: Arc::clone(&self.manifest_file),
            fs_directory_path: self.fs_directory_path.clone(),
        }
    }
}

async fn load_manifest(
    manifest_file: &mut fs::File,
    manifest: &mut HashMap<u128, InMemoryFile>,
    process_state: &mut HashMap<ProcessId, InMemoryFile>,
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
                if let Some(process) = entry.process {
                    process_state.insert(
                        process,
                        InMemoryFile {
                            hash: entry.file_hash,
                            file_type: entry.file_type,
                            hasher: Hasher::new(),
                            file_length: entry.file_length,
                        },
                    );
                } else {
                manifest.insert(
                    entry.file_uuid,
                    InMemoryFile {
                        hash: entry.file_hash,
                        file_type: entry.file_type,
                        hasher: Hasher::new(),
                        file_length: entry.file_length,
                    },
                );
                }
                // move to the next position after the metadata,
                current_position += 8 + metadata_length as u64;
            }
            Ok(ManifestRecord::Delete(delete)) => {
                if delete != 0 {
                    // pmwrite case special, doublecheck process/hash index case
                    manifest.remove(&delete);
                }

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

async fn verify_manifest(
    manifest: &mut HashMap<u128, InMemoryFile>,
    lfs_directory_path: &PathBuf,
    hash_index: &mut HashMap<[u8; 32], u128>,
) -> tokio::io::Result<()> {
    for (uuid, in_memory_file) in manifest.iter_mut() {
        match &in_memory_file.file_type {
            FileType::Appendable => {
                let mut file_path = lfs_directory_path.clone();
                file_path.push(uuid.to_string());

                match fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&file_path)
                    .await
                {
                    Ok(mut file) => {
                        if let Err(e) = file.set_len(in_memory_file.file_length).await {
                            println!("Failed to truncate file {}: {}", file_path.display(), e);
                        }

                        let mut buffer = vec![0u8; in_memory_file.file_length as usize];
                        file.seek(SeekFrom::Start(0)).await?;
                        file.read_exact(&mut buffer).await?;
                        in_memory_file.hasher.update(&buffer);

                        hash_index.insert(in_memory_file.hash, *uuid);
                    }
                    Err(e) => {
                        println!("Failed to open file {}: {}", file_path.display(), e);
                    }
                }
            }
            FileType::Immutable => {
                hash_index.insert(in_memory_file.hash, *uuid);
            },
            FileType::Writeable => {
                //  todo chunking, replay
                hash_index.insert(in_memory_file.hash, *uuid);
            }
        }
    }

    Ok(())
}