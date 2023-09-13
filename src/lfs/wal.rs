use blake3::Hasher;
///  append-only log file with commited changes, deletes, backups
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;
use tokio::io::{self, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::sync::RwLock;

use super::manifest::Manifest;

//   ON-DISK, WAL
#[derive(Serialize, Deserialize)]
pub enum WALRecord {
    Chunk(ChunkEntry), // wrote bytes to wal
    Delete(ToDelete),  // delete on-disk file
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ChunkEntry {
    file_uuid: u128,
    chunk_range: (u64, u64),
    chunk_hash: [u8; 32],
    //  data: Vec<u8>, data is kept separately after the chunk_entry
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ToDelete {
    Mutable(u128),
    Immutable([u8; 32]),
}

// IN-MEMORY, WAL
// note: store offsets into wal, not actual data?
//       can also just keep in memory until flush_call or backups_complete.

#[derive(Debug, Clone)]
pub struct WAL {
    pub log_file: Arc<RwLock<fs::File>>,
    pub wal: Arc<RwLock<HashMap<u128, InMemoryChunks>>>,
    pub to_delete: Arc<RwLock<Vec<ToDelete>>>,
    lfs_directory_path: PathBuf,
}
#[derive(Debug, Clone)]
pub struct InMemoryChunks {
    pub hasher: Hasher,
    pub data: Vec<u8>,
}

impl WAL {
    pub async fn load(log_file: fs::File, lfs_directory_path: &PathBuf) -> io::Result<Self> {
        let mut wal = HashMap::new();
        let mut log_file = log_file;
        let mut to_delete = Vec::new();

        load_wal(&mut log_file, &mut wal, &mut to_delete).await?;

        Ok(Self {
            wal: Arc::new(RwLock::new(wal)),
            to_delete: Arc::new(RwLock::new(to_delete)),
            log_file: Arc::new(RwLock::new(log_file)),
            lfs_directory_path: lfs_directory_path.clone(),
        })
    }

    pub async fn add_delete(&self, to_delete: ToDelete) -> io::Result<()> {
        let mut log_file = self.log_file.write().await;

        let record = WALRecord::Delete(to_delete.clone());
        let serialized_entry = bincode::serialize(&record).unwrap();
        let entry_length = serialized_entry.len() as u64;
        log_file.write_all(&entry_length.to_le_bytes()).await?;
        log_file.write_all(&serialized_entry).await?;

        self.to_delete.write().await.push(to_delete);
        Ok(())
    }

    pub async fn _flush(&self, manifest: Manifest) -> io::Result<()> {
        {
            let to_delete: tokio::sync::RwLockReadGuard<'_, Vec<ToDelete>> =
                self.to_delete.read().await;
            if to_delete.is_empty() {
                //  println!("empty delete log!");
                return Ok(());
            }
        }

        let mut log_file = self.log_file.write().await;
        let mut wal = self.wal.write().await;
        let mut to_delete: tokio::sync::RwLockWriteGuard<'_, Vec<ToDelete>> =
            self.to_delete.write().await;
        //  println!("deleting files: {:?}", to_delete);

        for del in to_delete.iter() {
            match del {
                ToDelete::Mutable(uuid) => {
                    let file_path = self.lfs_directory_path.join(uuid.to_string());
                    let _ = fs::remove_file(&file_path).await;
                    let _ = manifest.add_delete(del).await;
                }
                ToDelete::Immutable(file_hash) => {
                    let file_path = self.lfs_directory_path.join(hex::encode(file_hash));
                    let _ = fs::remove_file(&file_path).await;
                    let _ = manifest.add_delete(del).await;
                }
            }
        }
        wal.clear();
        to_delete.clear();
        log_file.set_len(0).await?;
        Ok(())
    }

    pub async fn _append_entry_to_wal(
        log_file: &mut fs::File,
        entry: &ChunkEntry,
        data: &[u8],
    ) -> Result<u64, io::Error> {
        let wal_position = log_file.metadata().await?.len();

        let record = WALRecord::Chunk(entry.clone());

        let serialized_entry = bincode::serialize(&record).unwrap();
        let entry_length = serialized_entry.len() as u64;
        let data_length = data.len() as u64;

        log_file.write_all(&entry_length.to_le_bytes()).await?; // write the metadata length prefix
        log_file.write_all(&serialized_entry).await?; // write the serialized metadata
        log_file.write_all(&data_length.to_le_bytes()).await?; // write the data length
        log_file.write_all(data).await?; // write the data

        // return the location where the data starts in the WAL
        Ok(wal_position + (8 + serialized_entry.len() as u64))
    }

    async fn _get_chunk_data(
        log_file: &mut fs::File,
        wal_position: u64,
    ) -> Result<Vec<u8>, io::Error> {
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

    // read, append
}

async fn load_wal(
    log_file: &mut fs::File,
    _wal: &mut HashMap<u128, InMemoryChunks>,
    to_delete: &mut Vec<ToDelete>,
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
        let record_length = u64::from_le_bytes(length_buffer) as usize;

        // Read serialized metadata
        let mut record_buffer = vec![0u8; record_length];
        log_file.read_exact(&mut record_buffer).await?;
        let record: Result<WALRecord, _> = bincode::deserialize(&record_buffer);

        match record {
            Ok(WALRecord::Chunk(_chunk_entry)) => {
                // get or create an entry for file_uuid in manifest
                //  let in_memory_file = manifest
                //      .entry(chunk_entry.file_uuid)
                //      .or_insert(InMemoryFile::default());

                let mut data_length_buffer = [0u8; 8];
                log_file.read_exact(&mut data_length_buffer).await?;
                let data_length = u64::from_le_bytes(data_length_buffer) as usize;

                // skip or read in entry data.
                current_position += 8 + record_length as u64 + 8;
                current_position += data_length as u64;
            }
            Ok(WALRecord::Delete(del)) => {
                to_delete.push(del);
            }
            // backups etc
            Err(_) => {
                println!("failed to deserialize WALRecord.");
                break;
            }
        }
    }

    // truncate the WAL file to the current position
    log_file.set_len(current_position).await?;
    Ok(())
}
