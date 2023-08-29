use std::{collections::HashMap, sync::Arc};
use serde::{Serialize, Deserialize};
use thiserror::Error;
use tokio::sync::RwLock;
use ring::digest;

pub const PROCESS_MANAGER_ID: u64 = 0;
pub const KERNEL_ID: u64 = 1;
pub const FILESYSTEM_ID: u64 = 57005;
pub const HTTP_SERVER_ID: u64 = 48879;
pub const HTTP_CLIENT_ID: u64 = 51966;
pub const LFS_ID: u64 = 47806;

pub type MessageSender = tokio::sync::mpsc::Sender<KernelMessage>;
pub type MessageReceiver = tokio::sync::mpsc::Receiver<KernelMessage>;

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
pub struct ProcessAddress {
    pub node: String,
    pub id: u64,
    pub name: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessReference {
    pub node: String,
    pub identifier: ProcessIdentifier,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProcessIdentifier {
    Id(u64),
    Name(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TransitMessage {
    Request(TransitRequest),
    Response(TransitPayload),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransitRequest {
    pub is_expecting_response: bool,
    pub payload: TransitPayload,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransitPayload {
    pub source: ProcessReference,
    // pub json: Option<serde_json::Value>,
    pub json: Option<String>,
    pub bytes: TransitPayloadBytes,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TransitPayloadBytes {
    None,
    Some(Vec<u8>),
    // AttachCircumvented,
    Circumvent(Vec<u8>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UqbarError {
    // pub source: ProcessNode,
    pub source: ProcessReference,
    pub timestamp: u64,
    pub payload: UqbarErrorPayload,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UqbarErrorPayload {
    pub kind: String,
    pub message: serde_json::Value,
    pub context: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KernelMessage {
    pub id: u64,
    pub target: ProcessReference,
    pub rsvp: Rsvp,
    pub message: Result<TransitMessage, UqbarError>,
}

//  kernel sets in case, e.g.,
//   A requests response from B does not request response from C
//   -> kernel sets `Some(A) = Rsvp` for B's request to C
// pub type Rsvp = Option<ProcessNode>;
pub type Rsvp = Option<ProcessReference>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BootOutboundRequest {
    pub target_process: ProcessIdentifier,
    pub json: Option<String>,
    pub bytes: TransitPayloadBytes,
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

impl std::fmt::Display for ProcessIdentifier {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            ProcessIdentifier::Name(n) => write!(f, "{}", n),
            ProcessIdentifier::Id(i) => write!(f, "{}", i),
        }
    }
}

impl std::fmt::Display for ProcessReference {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "{{ node: {}, identifier: {} }}",
            self.node,
            self.identifier,
        )
    }
}
impl std::fmt::Display for TransitMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            TransitMessage::Request(request) => {
                write!(
                    f,
                    "Request({})",
                    request,
                )
            },
            TransitMessage::Response(payload) => {
                write!(
                    f,
                    "Response({})",
                    payload,
                )
            },
        }
    }
}
impl std::fmt::Display for TransitRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "{{ is_expecting_response: {}, payload: {} }}",
            self.is_expecting_response,
            self.payload,
        )
    }
}
impl std::fmt::Display for TransitPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let json: serde_json::Value = match self.json {
            None => serde_json::Value::Null,
            Some(ref json_string) => {
                match serde_json::to_value(json_string) {
                    Ok(json) => json,
                    Err(e) => serde_json::json!({"error": format!("{}", e)}),
                }
            },
        };
        write!(
            f,
            "{{ source: {:?}, json: {}, bytes: {} }}",
            self.source,
            json,
            self.bytes,
        )
    }
}
impl std::fmt::Display for TransitPayloadBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                TransitPayloadBytes::None => "None",
                TransitPayloadBytes::Some(_) => "Some(<elided>)",
                TransitPayloadBytes::Circumvent(_) => "Circumvent(<elided>)",
            },
        )
    }
}
impl std::fmt::Display for KernelMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let message = match self.message {
            Ok(ref m) => format!("{}", m),
            Err(ref e) => format!("{:?}", e),
        };
        write!(
            f,
            "{{ id: {}, target: {}, rsvp: {:?}, message: {} }}",
            self.id,
            self.target,
            self.rsvp,
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
    ListRunningProcesses { processes: Vec<String> },
    PersistState([u8; 32]),
}

#[derive(Debug, Serialize, Deserialize)]
pub enum KernelRequest {
    StartProcess {
        id: u64,
        name: Option<String>,
        wasm_bytes_uri: String,
        send_on_panic: SendOnPanic,
    },
    StopProcess { id: u64 },
    RegisterProcess { id: u64, name: String },
    UnregisterProcess { id: u64 },
}
#[derive(Debug, Serialize, Deserialize)]
pub enum KernelResponse {
    StartProcess(ProcessMetadata),
    StopProcess { id: u64 },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessMetadata {
    pub our: ProcessAddress,
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
            FileSystemError::LFSError { .. } => "LFSErrror",
        }
    }
}
#[derive(Error, Debug, Serialize, Deserialize)]
pub enum FileSystemError {
    //  bad input from user
    #[error("Malformed URI: {uri}. Problem with {bad_part_name}: {:?}.", bad_part)]
    BadUri { uri: String, bad_part_name: String,  bad_part: Option<String>, },
    #[error("JSON payload could not be parsed to FileSystemRequest: {error}. Got {:?}.", json)]
    BadJson { json: String, error: String, },
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
    #[error("LFS error: {error}.")]
    LFSError { error: String },
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
    QueueMessage {
        target_node: Option<String>,
        target_process: ProcessIdentifier,
        json: Option<String>,
    },
    RunQueue,
}
