use blake3::Hasher;
///  append-only log file of your local filesystem (and backups)
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;
use tokio::io::{self, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::sync::RwLock;
use super::wal::{ToDelete};
use crate::types::FileSystemError;

//   ON-DISK, MANIFEST
#[derive(Serialize, Deserialize, Clone)]
pub enum ManifestRecord {
    BackupI(BackupImmutable),
    BackupA(BackupAppendable),
    Delete(u128),
}

#[derive(Serialize, Deserialize, Clone)]
pub struct BackupImmutable {
    pub file_uuid: u128,
    pub file_hash: [u8; 32],
    pub file_length: u64,
    pub backup: Vec<[u8; 32]>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct BackupAppendable {
    pub file_uuid: u128,
    pub file_hash: [u8; 32],
    // file_hashes vec + ranges vs in-mem?
    pub file_length: u64,
    pub backup: Vec<[u8; 32]>,
}

// IN-MEMORY, MANIFEST

#[derive(Debug, Clone)]
pub enum FileType {
    Appendable,
    Immutable,
}
#[derive(Debug, Clone)]
pub struct InMemoryFile {
    pub hash: [u8; 32], // content addressed hash (naive)
    pub file_type: FileType,
    pub hasher: Hasher,
    pub file_length: u64,
}

pub struct Manifest {
    pub manifest: Arc<RwLock<HashMap<u128, InMemoryFile>>>,
    pub hash_index: Arc<RwLock<HashMap<[u8; 32], u128>>>,
    pub manifest_file: Arc<RwLock<fs::File>>,
}

impl Manifest {
    pub async fn load(manifest_file: fs::File, lfs_directory_path: &PathBuf) -> io::Result<Self> {
        let mut manifest = HashMap::new();
        let mut hash_index = HashMap::new();
        let mut manifest_file = manifest_file;

        load_manifest(&mut manifest_file, &mut manifest).await?;

        verify_manifest(&mut manifest, &lfs_directory_path, &mut hash_index).await?;

        Ok(Self {
            manifest: Arc::new(RwLock::new(manifest)),
            hash_index: Arc::new(RwLock::new(hash_index)),
            manifest_file: Arc::new(RwLock::new(manifest_file)),
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

    pub async fn add_immutable(
        &self,
        entry: &ManifestRecord,
    ) -> Result<(), FileSystemError> {
        let mut manifest_file = self.manifest_file.write().await;
        let serialized_entry = bincode::serialize(&entry).unwrap();
        let entry_length = serialized_entry.len() as u64;

        let mut buffer = Vec::new();
        buffer.extend_from_slice(&entry_length.to_le_bytes()); // add the metadata length prefix
        buffer.extend_from_slice(&serialized_entry);           // add the serialized metadata

        if let Err(e) = manifest_file.write_all(&buffer).await {
            return Err(FileSystemError::LFSError {
                error: format!("write failed: {}", e),
            });
        }

        if let ManifestRecord::BackupI(immutable) = entry {
            let mut wmanifest = self.manifest.write().await;
            let mut whash_index = self.hash_index.write().await;

            wmanifest.insert(
                immutable.file_uuid,
                InMemoryFile {
                    hash: immutable.file_hash,
                    file_type: FileType::Immutable,
                    hasher: Hasher::new(),
                    file_length: immutable.file_length,
                },
            );

            whash_index.insert(immutable.file_hash, immutable.file_uuid);
        }

        Ok(())
    }

    pub async fn add_append(
        &self,
        entry: &ManifestRecord,
    ) -> Result<(), FileSystemError> {
        let mut manifest_file = self.manifest_file.write().await;
        let serialized_entry = bincode::serialize(&entry).unwrap();
        let entry_length = serialized_entry.len() as u64;

        let mut buffer = Vec::new();
        buffer.extend_from_slice(&entry_length.to_le_bytes()); // add the metadata length prefix
        buffer.extend_from_slice(&serialized_entry);           // add the serialized metadata

        if let Err(e) = manifest_file.write_all(&buffer).await {
            return Err(FileSystemError::LFSError {
                error: format!("write failed: {}", e),
            });
        }

        // note todo: in-memory structures not updated here, outside of function.W
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
            return Ok(());
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
            hash_index: Arc::clone(&self.hash_index),
            manifest_file: Arc::clone(&self.manifest_file),
        }
    }
}

async fn load_manifest(
    manifest_file: &mut fs::File,
    manifest: &mut HashMap<u128, InMemoryFile>,
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
            Ok(ManifestRecord::BackupI(immutable)) => {
                manifest.insert(
                    immutable.file_uuid,
                    InMemoryFile {
                        hash: immutable.file_hash,
                        file_type: FileType::Immutable,
                        hasher: Hasher::new(),
                        file_length: immutable.file_length,
                    },
                );
                // move to the next position after the metadata,
                current_position += 8 + metadata_length as u64;
            }
            Ok(ManifestRecord::BackupA(appendable)) => {
                manifest.insert(
                    appendable.file_uuid,
                    InMemoryFile {
                        hash: appendable.file_hash,
                        file_type: FileType::Appendable,
                        hasher: Hasher::new(), // populate hasher afterwards
                        file_length: appendable.file_length,
                    },
                );

                current_position += 8 + metadata_length as u64;
            }
            Ok(ManifestRecord::Delete(delete)) => {
                if delete != 0 {
                    // pmwrite case special
                    manifest.remove(&delete);
                }
                
                current_position += 8 + metadata_length as u64;
            }

            Err(_) => {
                // faulty entry, remove, or maybe skip.
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
            }
        }
    }

    Ok(())
}
