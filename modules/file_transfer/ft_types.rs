use serde::{Serialize, Deserialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessAddress {
    pub node: String,
    pub id: u64,
    pub name: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq, Hash)]
pub struct ProcessReference {
    pub node: String,
    pub identifier: ProcessIdentifier,
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq, Hash)]
pub enum ProcessIdentifier {
    Id(u64),
    Name(String),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TransitPayloadBytes {
    None,
    Some(Vec<u8>),
    Circumvent(Vec<u8>),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestOnPanic {
    pub target: ProcessReference,
    pub json: Option<String>,
    pub bytes: TransitPayloadBytes,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SendOnPanic {
    None,
    Restart,
    Requests(Vec<RequestOnPanic>),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProcessManagerCommand {
    Initialize { jwt_secret_bytes: Option<Vec<u8>> },
    Start { name: Option<String>, wasm_bytes_uri: String, send_on_panic: SendOnPanic },
    Stop { id: u64 },
    Restart { id: u64 },
    ListRegisteredProcesses,
    PersistState,
    RebootStart { id: u64, name: Option<String>, wasm_bytes_uri: String, send_on_panic: SendOnPanic },  //  TODO: remove
}
#[derive(Debug, Serialize, Deserialize)]
pub enum ProcessManagerResponse {
    Initialize,
    Start { id: u64, name: Option<String> },
    ListRegisteredProcesses { processes: Vec<String> },
    PersistState([u8; 32]),
}

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
    pub file_hash: [u8; 32],
    pub start: u64,
    pub length: u64,
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

// #[derive(Error, Debug, Serialize, Deserialize)]
// pub enum NetworkingError {
//     #[error("Peer is offline or otherwise unreachable")]
//     PeerOffline,
//     #[error("Message delivery failed due to timeout")]
//     MessageTimeout,
//     #[error("Some bug in the networking code")]
//     NetworkingBug,
// }

#[derive(Clone, Debug, Serialize, Deserialize, Eq, Hash, PartialEq)]
pub struct FileTransferKey {
    pub client: String,
    pub server: String,
    pub file_hash: [u8; 32],  // ?
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileTransferMetadata {
    pub key: FileTransferKey,
    pub chunk_size: u64,
    pub number_pieces: u64,
    pub number_bytes: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum FileTransferRequest {
    Initialize,                                                                                                 //  pm to client
    GetFile { target_node: String, file_hash: [u8; 32], chunk_size: u64, resume_file_hash: Option<[u8; 32]> },  //  user to client to client_worker
    Start { file_hash: [u8; 32], chunk_size: u64 },                                                             //  client_worker to server
    StartWorker { client_worker: ProcessReference, file_hash: [u8; 32], chunk_size: u64 },                      //  server to server_worker
    GetPiece { piece_number: u64 },                                                                             //  client_worker to server_worker
    Done,                                                                                                       //  client_worker to server_worker
    DisplayOngoing,                                                                                             //  user to client
    UpdateClientState { current_file_hash: [u8; 32] },                                                          //  client_worker to client
    // Cancel { key: FileTransferKey, is_cancel_both: bool, reason: String },
    // ReadDir { target_node: String, uri_string: String, }  //  from user to requester to server
}

#[derive(Debug, Serialize, Deserialize)]
pub enum FileTransferResponse {
    Start(FileTransferMetadata),     //  server_worker to client_worker
    GetPiece { piece_number: u64 },  //  server_worker to client_worker
    UpdateClientState,               //  client to client_worker
    // ReadDir(Vec<FileSystemMetadata>),  //  from server to requester
}

pub type ClientState = HashMap<ProcessReference, ClientStateValue>;
#[derive(Debug, Serialize, Deserialize)]
pub struct ClientStateValue {
    // pub worker: ProcessReference,  // key
    pub target_node: String,
    pub file_hash: [u8; 32],
    pub chunk_size: u64,
    pub current_file_hash: Option<[u8; 32]>,
}
pub struct ClientWorkerState {
    pub metadata: FileTransferMetadata,
    pub current_file_hash: Option<[u8; 32]>,
    pub next_piece_number: u64,
}
pub struct ServerWorkerState {
    pub client_worker: ProcessReference,  //  TODO: needed?
    pub metadata: FileTransferMetadata,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileTransferContext {
    pub key: FileTransferKey,
    pub additional: FileTransferAdditionalContext,
}
#[derive(Debug, Serialize, Deserialize)]
pub enum FileTransferAdditionalContext {
    Empty,
    Metadata { chunk_size: u64 },
    Piece { piece_number: u64 },
}

pub enum MessageHandledStatus {
    ReadyForNext,
    Done,
}
