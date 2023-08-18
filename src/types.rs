use std::{collections::HashMap, sync::Arc};
use serde::{Serialize, Deserialize};
use thiserror::Error;
use tokio::sync::RwLock;
use ring::digest;

pub type MessageSender = tokio::sync::mpsc::Sender<WrappedMessage>;
pub type MessageReceiver = tokio::sync::mpsc::Receiver<WrappedMessage>;

pub type PrintSender = tokio::sync::mpsc::Sender<Printout>;
pub type PrintReceiver = tokio::sync::mpsc::Receiver<Printout>;

pub type DebugSender = tokio::sync::mpsc::Sender<DebugCommand>;
pub type DebugReceiver = tokio::sync::mpsc::Receiver<DebugCommand>;

pub type OnchainPKI = Arc<RwLock<HashMap<String, Identity>>>;

#[derive(Debug, Serialize, Deserialize)]
pub struct Registration {
    pub username: String,
    pub password: String,
    pub address: String,
    pub direct: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Identity {
    pub name: String,
    pub address: String,
    pub networking_key: String,
    pub ws_routing: Option<(String, u16)>,
    pub allowed_routers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityTransaction {
    pub from: String,
    pub signature: Option<String>,
    pub to: String, // contract address
    pub town_id: u32,
    pub calldata: Identity,
    pub nonce: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessNode {
    pub node: String,
    pub process: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Payload {
    pub json: Option<serde_json::Value>,
    pub bytes: PayloadBytes,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PayloadBytes {
    pub circumvent: Circumvent,
    pub content: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Circumvent {
    False,         //  do not circumvent
    Send,          //  circumvent with these bytes
    Circumvented,  //  we were circumvented
    Receive,       //  copy circumventing bytes into this message
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WrappedMessage {
    pub id: u64,
    pub target: ProcessNode,
    pub rsvp: Rsvp,
    pub message: Result<Message, UqbarError>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UqbarError {
    pub source: ProcessNode,
    pub timestamp: u64,
    pub content: UqbarErrorContent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UqbarErrorContent {
    pub kind: String,
    pub message: serde_json::Value,
    pub context: serde_json::Value,
}

//  kernel sets in case, e.g.,
//   A requests response from B does not request response from C
//   -> kernel sets `Some(A) = Rsvp` for B's request to C
pub type Rsvp = Option<ProcessNode>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub source: ProcessNode,
    pub content: MessageContent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MessageContent {
    pub message_type: MessageType,
    pub payload: Payload,
}

// TODO this is a hack to get around the fact that serde_json::Value
//      is not serializable using bincode.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BinSerializablePayload {
    pub json: Option<Vec<u8>>,
    pub bytes: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BinSerializableWrappedMessage {
    pub id: u64,
    pub target_process: String,
    //  target node assigned by runtime to "our"
    //  rsvp assigned by runtime (as None)
    pub message: BinSerializableMessage,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BinSerializableMessage {
    //  source assigned by runtime
    pub content: BinSerializableMessageContent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BinSerializableMessageContent {
    pub message_type: MessageType,
    pub payload: BinSerializablePayload,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MessageType {
    Request(bool),
    Response,
}

#[derive(Error, Debug, Serialize, Deserialize)]
pub enum NetworkingError {
    #[error("Peer is offline or otherwise unreachable")]
    PeerOffline,
    #[error("Message delivery failed due to timeout")]
    MessageTimeout,
    #[error("Some bug in the networking code")]
    NetworkingBug,
}
impl std::fmt::Display for ProcessNode {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "{{ node: {}, process: {} }}",
            self.node,
            self.process,
        )
    }
}

impl std::fmt::Display for PayloadBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let bytes_string = match self.content {
            Some(_) => "Some(<elided>)",
            None => "None",
        };
        write!(
            f,
            "{{ circumvent: {:?}, bytes: {} }}",
            self.circumvent,
            bytes_string,
        )
    }
}

impl std::fmt::Display for Payload {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "{{ json: {:?}, bytes: {} }}",
            self.json,
            self.bytes,
        )
    }
}

impl std::fmt::Display for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "{{ source: {}, content: {} }}",
            self.source,
            self.content,
        )
    }
}

impl std::fmt::Display for MessageContent {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "{{ message_type: {:?}, payload: {} }}",
            self.message_type,
            self.payload,
        )
    }
}

impl std::fmt::Display for WrappedMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let rsvp = match self.rsvp {
            Some(ref rsvp) => format!("{}", rsvp),
            None => "None".into(),
        };
        let message = match self.message {
            Ok(ref m) => format!("{}", m),
            Err(ref e) => format!("{:?}", e),
        };
        write!(
            f,
            "{{ id: {}, target: {}, rsvp: {}, message: {} }}",
            self.id,
            self.target,
            rsvp,
            message,
        )
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum DebugCommand {
    Toggle,
    Step,
}

/// A terminal printout. Verbosity level is from low to high, and for
/// now, only 0 and 1 are used. Level 0 is always printed, level 1 is
/// only printed if the terminal is in verbose mode. Numbers greater
/// than 1 are reserved for future use and will be ignored for now.
pub struct Printout {
    pub verbosity: u8,
    pub content: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestOnPanic {
    pub target: ProcessNode,
    pub payload: Payload,
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
    Start { process_name: String, wasm_bytes_uri: String, send_on_panic: SendOnPanic },
    Stop { process_name: String },
    Restart { process_name: String },
    ListRunningProcesses,
}
#[derive(Debug, Serialize, Deserialize)]
pub enum ProcessManagerResponse {
    ListRunningProcesses { processes: Vec<String> },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum KernelRequest {
    StartProcess { process_name: String, wasm_bytes_uri: String, send_on_panic: SendOnPanic },
    StopProcess { process_name: String },
}
#[derive(Debug, Serialize, Deserialize)]
pub enum KernelResponse {
    StartProcess(ProcessMetadata),
    StopProcess { process_name: String },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessMetadata {
    pub our: ProcessNode,
    pub wasm_bytes_uri: String,  // TODO: for use in restarting erroring process, ala midori
    pub send_on_panic: SendOnPanic,
}

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

// keygen types
pub const CREDENTIAL_LEN: usize = digest::SHA256_OUTPUT_LEN;
pub type DiskKey = [u8; CREDENTIAL_LEN];

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SequentializeRequest {
    QueueMessage { target_node: Option<String>, target_process: String, json: Option<String> },
    RunQueue,
}
