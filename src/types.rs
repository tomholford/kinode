use ring::digest;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use thiserror::Error;
use tokio::sync::RwLock;

//
// internal message pipes between kernel and runtime modules
//

// keeps the from address so we know where to pipe error
pub type NetworkErrorSender = tokio::sync::mpsc::Sender<WrappedNetworkError>;
pub type NetworkErrorReceiver = tokio::sync::mpsc::Receiver<WrappedNetworkError>;

pub type MessageSender = tokio::sync::mpsc::Sender<KernelMessage>;
pub type MessageReceiver = tokio::sync::mpsc::Receiver<KernelMessage>;

pub type PrintSender = tokio::sync::mpsc::Sender<Printout>;
pub type PrintReceiver = tokio::sync::mpsc::Receiver<Printout>;

pub type DebugSender = tokio::sync::mpsc::Sender<DebugCommand>;
pub type DebugReceiver = tokio::sync::mpsc::Receiver<DebugCommand>;

//
// types used for UQI: uqbar's identity system
//

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

//
// process-facing kernel types, used for process
// management and message-passing
// matches types in uqbar.wit
//

pub type Context = String; // JSON-string

#[derive(Clone, Debug, Eq, Hash, Serialize, Deserialize)]
pub enum ProcessId {
    Id(u64),
    Name(String),
}

impl PartialEq for ProcessId {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (ProcessId::Id(i1), ProcessId::Id(i2)) => i1 == i2,
            (ProcessId::Name(s1), ProcessId::Name(s2)) => s1 == s2,
            _ => false,
        }
    }
}
impl PartialEq<&str> for ProcessId {
    fn eq(&self, other: &&str) -> bool {
        match self {
            ProcessId::Id(_) => false,
            ProcessId::Name(s) => s == other,
        }
    }
}
impl PartialEq<u64> for ProcessId {
    fn eq(&self, other: &u64) -> bool {
        match self {
            ProcessId::Id(i) => i == other,
            ProcessId::Name(_) => false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Address {
    pub node: String,
    pub process: ProcessId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Payload {
    pub mime: Option<String>, // MIME type
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub inherit: bool,
    pub expects_response: bool,
    pub ipc: Option<String>,      // JSON-string
    pub metadata: Option<String>, // JSON-string
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Response {
    pub ipc: Option<String>,      // JSON-string
    pub metadata: Option<String>, // JSON-string
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Message {
    Request(Request),
    Response((Result<Response, UqbarError>, Option<Context>)),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UqbarError {
    pub kind: String,
    pub message: Option<String>, // JSON-string
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkError {
    pub kind: NetworkErrorKind,
    pub target: Address, // what the message was trying to reach
    pub message: Message,
    pub payload: Option<Payload>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NetworkErrorKind {
    Offline,
    Timeout,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum OnPanic {
    None,
    Restart,
    Requests(Vec<(Address, Request, Option<Payload>)>),
}

//
// kernel types that runtime modules use
//

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessMetadata {
    pub our: Address,
    pub wasm_bytes_handle: u128,
    pub on_panic: OnPanic,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KernelMessage {
    pub id: u64,
    pub source: Address,
    pub target: Address,
    pub rsvp: Rsvp,
    pub message: Message,
    pub payload: Option<Payload>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WrappedNetworkError {
    pub id: u64,
    pub source: Address,
    pub error: NetworkError,
}

/// A terminal printout. Verbosity level is from low to high, and for
/// now, only 0 and 1 are used. Level 0 is always printed, level 1 is
/// only printed if the terminal is in verbose mode. Numbers greater
/// than 1 are reserved for future use and will be ignored for now.
pub struct Printout {
    pub verbosity: u8,
    pub content: String,
}

//  kernel sets in case, e.g.,
//   A requests response from B does not request response from C
//   -> kernel sets `Some(A) = Rsvp` for B's request to C
pub type Rsvp = Option<Address>;

//
//  boot/startup specific types???
//

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SequentializeRequest {
    QueueMessage(QueueMessage),
    RunQueue,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct QueueMessage {
    pub target: ProcessId,
    pub request: Request,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BootOutboundRequest {
    pub target_process: ProcessId,
    pub json: Option<String>,
    pub bytes: Option<Vec<u8>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum DebugCommand {
    Toggle,
    Step,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum KernelCommand {
    StartProcess {
        name: Option<String>,
        wasm_bytes_handle: u128,
        on_panic: OnPanic,
    },
    KillProcess(ProcessId), // this is extrajudicial killing: we might lose messages!
    RebootProcess {
        // kernel only
        process_id: ProcessId,
        wasm_bytes_handle: u128,
        on_panic: OnPanic,
    },
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum KernelResponse {
    StartedProcess(ProcessMetadata),
    KilledProcess(ProcessId),
}

//
// runtime-module-specific types
//

//
// filesystem.rs types
//

#[derive(Serialize, Deserialize, Debug)]
pub enum FsAction {
    Write,
    Replace(FileHash),
    WriteOffset((FileHash, u64)),
    Append(Option<FileHash>),
    Read(FileHash),
    ReadChunk(ReadChunkRequest),
    Delete(FileHash),
    Length(FileHash),
    SetLength((FileHash, u64)),
    GetState,
    SetState,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ReadChunkRequest {
    pub file: FileHash,
    pub start: u64,
    pub length: u64,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum FsResponse {
    Write(FileHash),
    Read(FileHash),
    ReadChunk(FileHash),
    Append(FileHash),
    Delete(FileHash),
    Length(u64),
    GetState,
    SetState,
}

impl VfsError {
    pub fn kind(&self) -> &str {
        match *self {
            VfsError::BadDescriptor => "BadDescriptor",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum VfsError {
    BadDescriptor,
}

impl FileSystemError {
    pub fn kind(&self) -> &str {
        match *self {
            FileSystemError::BadUri { .. } => "BadUri",
            FileSystemError::BadJson { .. } => "BadJson",
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
    BadUri {
        uri: String,
        bad_part_name: String,
        bad_part: Option<String>,
    },
    #[error(
        "JSON payload could not be parsed to FileSystemRequest: {error}. Got {:?}.",
        json
    )]
    BadJson { json: String, error: String },
    #[error("Bytes payload required for {action}.")]
    BadBytes { action: String },
    #[error("{process_name} not allowed to access {attempted_dir}. Process may only access within {sandbox_dir}.")]
    IllegalAccess {
        process_name: String,
        attempted_dir: String,
        sandbox_dir: String,
    },
    #[error("Already have {path} opened with mode {:?}.", mode)]
    AlreadyOpen { path: String, mode: FileSystemMode },
    #[error("Don't have {path} opened with mode {:?}.", mode)]
    NotCurrentlyOpen { path: String, mode: FileSystemMode },
    //  path or underlying fs problems
    #[error("Failed to join path: base: '{base_path}'; addend: '{addend}'.")]
    BadPathJoin { base_path: String, addend: String },
    #[error("Failed to create dir at {path}: {error}.")]
    CouldNotMakeDir { path: String, error: String },
    #[error("Failed to read {path}: {error}.")]
    ReadFailed { path: String, error: String },
    #[error("Failed to write {path}: {error}.")]
    WriteFailed { path: String, error: String },
    #[error("Failed to open {path} for {:?}: {error}.", mode)]
    OpenFailed {
        path: String,
        mode: FileSystemMode,
        error: String,
    },
    #[error("Filesystem error while {what} on {path}: {error}.")]
    FsError {
        what: String,
        path: String,
        error: String,
    },
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
    Open {
        uri_string: String,
        mode: FileSystemMode,
    },
    Close {
        uri_string: String,
        mode: FileSystemMode,
    },
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

pub type FileHash = u128;

#[derive(Debug, Serialize, Deserialize)]
pub enum VfsRequest {
    Add {
        full_path: String,
        entry_type: AddEntryType,
    },
    Rename {
        full_path: String,
        new_full_path: String,
    },
    Delete {
        full_path: String,
    },
    GetPath {
        hash: FileHash,
    },
    GetEntry {
        full_path: String,
    },
    GetFileChunk {
        full_path: String,
        offset: u64,
        length: u64,
    },
    WriteChunk {
        full_path: String,
        offset: u64,
        length: u64,
    },
    GetEntryLength {
        full_path: String,
    },
    //  wasi
    // FdAdd { fd: u32, entry_type: AddEntryType },
    // FdRename { fd: u32, new_full_path: String },
    FdDelete {
        fd: u32,
    },
    FdGetPath {
        fd: u32,
    },
    FdGetEntry {
        fd: u32,
    },
    FdDirStreamNext {
        stream_id: u32,
    },
    FdDirStreamDrop {
        stream_id: u32,
    },
    FdGetFileChunk {
        fd: u32,
        offset: u64,
        length: u64,
    },
    FdWriteChunk {
        fd: u32,
        offset: u64,
        length: u64,
    },
    FdGetEntryLength {
        fd: u32,
    },
    FdGetType {
        fd: u32,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum AddEntryType {
    Dir,
    NewFile, //  add a new file to lfs and add name in vfs
    ExistingFile { hash: FileHash }, //  link an existing file in lfs to a new name in vfs
             //  ...  //  symlinks?
}

#[derive(Debug, Serialize, Deserialize)]
pub enum GetEntryType {
    Dir,
    File,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum VfsResponse {
    Add {
        full_path: String,
    },
    Rename {
        new_full_path: String,
    },
    Delete {
        full_path: String,
    },
    GetPath {
        hash: FileHash,
        full_path: Option<String>,
    },
    GetEntry {
        full_path: String,
        children: Vec<String>,
    },
    GetFileChunk {
        full_path: String,
        offset: u64,
        length: u64,
    },
    WriteChunk {
        full_path: String,
        offset: u64,
        length: u64,
    },
    GetEntryLength {
        full_path: String,
        length: u64,
    },
    FdDelete {
        fd: u32,
    },
    FdGetPath {
        fd: u32,
        full_path: Option<String>,
    },
    FdGetEntry {
        fd: u32,
        stream_id: Option<u32>,
    },
    FdDirStreamNext {
        stream_id: u32,
        child: Option<String>,
    },
    FdDirStreamDrop {
        stream_id: u32,
    },
    FdGetFileChunk {
        fd: u32,
        offset: u64,
        length: u64,
    },
    FdWriteChunk {
        fd: u32,
        offset: u64,
        length: u64,
    },
    FdGetEntryLength {
        fd: u32,
        length: u64,
    },
    FdGetType {
        fd: u32,
        entry_type: GetEntryType,
    },
}

//
// http_client.rs types
//

#[derive(Debug, Serialize, Deserialize)]
pub struct HttpClientRequest {
    pub uri: String,
    pub method: String,
    pub headers: HashMap<String, String>,
    pub body: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HttpClientResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
}

#[derive(Error, Debug, Serialize, Deserialize)]
pub enum HttpClientError {
    #[error("http_client: rsvp is None but message is expecting response")]
    BadRsvp,
    #[error("http_client: no json in request")]
    NoJson,
    #[error(
        "http_client: JSON payload could not be parsed to HttpClientRequest: {error}. Got {:?}.",
        json
    )]
    BadJson { json: String, error: String },
    #[error("http_client: http method not supported: {:?}", method)]
    BadMethod { method: String },
    #[error("http_client: failed to execute request {:?}", error)]
    RequestFailed { error: String },
}

impl HttpClientError {
    pub fn kind(&self) -> &str {
        match *self {
            HttpClientError::BadRsvp { .. } => "BadRsvp",
            HttpClientError::NoJson { .. } => "NoJson",
            HttpClientError::BadJson { .. } => "BadJson",
            HttpClientError::BadMethod { .. } => "BadMethod",
            HttpClientError::RequestFailed { .. } => "RequestFailed",
        }
    }
}

//
// keygen types
//

pub const CREDENTIAL_LEN: usize = digest::SHA256_OUTPUT_LEN;
pub type DiskKey = [u8; CREDENTIAL_LEN];

//
// custom kernel displays
//

impl std::fmt::Display for KernelMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "{{ id: {}, source: {:?}, target: {:?}, rsvp: {:?}, message: {:?} }}",
            self.id, self.source, self.target, self.rsvp, self.message,
        )
    }
}

//
// http_server.rs types
//

#[derive(Debug, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Option<Vec<u8>>, // TODO does this use a lot of memory?
}

#[derive(Error, Debug, Serialize, Deserialize)]
pub enum HttpServerError {
    #[error("http_server: json is None")]
    NoJson,
    #[error("http_server: response not ok")]
    ResponseError,
    #[error("http_server: bytes are None")]
    NoBytes,
    #[error(
        "http_server: JSON payload could not be parsed to HttpClientRequest: {error}. Got {:?}.",
        json
    )]
    BadJson { json: String, error: String },
}

impl HttpServerError {
    pub fn kind(&self) -> &str {
        match *self {
            HttpServerError::NoJson { .. } => "NoJson",
            HttpServerError::NoBytes { .. } => "NoBytes",
            HttpServerError::BadJson { .. } => "BadJson",
            HttpServerError::ResponseError { .. } => "ResponseError",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JwtClaims {
    pub username: String,
    pub expiration: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WebSocketServerTarget {
    pub node: String,
    pub id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WebSocketPush {
    pub target: WebSocketServerTarget,
    pub is_text: Option<bool>,
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Address {
                    node,
                    process: ProcessId::Id(id),
                } => format!("{}/{}", node, id),
                Address {
                    node,
                    process: ProcessId::Name(name),
                } => format!("{}/{}", node, name),
            }
        )
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ServerAction {
    pub action: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum HttpServerMessage {
    WebSocketPush(WebSocketPush),
    ServerAction(ServerAction),
    WsRegister(WsRegister),                 // Coming from a proxy
    WsProxyDisconnect(WsProxyDisconnect),   // Coming from a proxy
    WsMessage(WsMessage),                   // Coming from a proxy
    EncryptedWsMessage(EncryptedWsMessage), // Coming from a proxy
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WsRegister {
    pub ws_auth_token: String,
    pub auth_token: String,
    pub channel_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WsProxyDisconnect {
    // Doesn't require auth because it's coming from the proxy
    pub channel_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WsMessage {
    pub ws_auth_token: String,
    pub auth_token: String,
    pub channel_id: String,
    pub target: Address,
    pub json: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncryptedWsMessage {
    pub ws_auth_token: String,
    pub auth_token: String,
    pub channel_id: String,
    pub target: Address,
    pub encrypted: String, // Encrypted JSON as hex with the 32-byte authentication tag appended
    pub nonce: String,     // Hex of the 12-byte nonce
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum WebSocketClientMessage {
    WsRegister(WsRegister),
    WsMessage(WsMessage),
    EncryptedWsMessage(EncryptedWsMessage),
}
// http_server End

// encryptor Start
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GetKeyAction {
    pub channel_id: String,
    pub public_key_hex: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DecryptAndForwardAction {
    pub channel_id: String,
    pub forward_to: Address, // node, process
    pub json: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncryptAndForwardAction {
    pub channel_id: String,
    pub forward_to: Address, // node, process
    pub json: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DecryptAction {
    pub channel_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncryptAction {
    pub channel_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum EncryptorMessage {
    GetKeyAction(GetKeyAction),
    DecryptAndForwardAction(DecryptAndForwardAction),
    EncryptAndForwardAction(EncryptAndForwardAction),
    DecryptAction(DecryptAction),
    EncryptAction(EncryptAction),
}
// encryptor End
