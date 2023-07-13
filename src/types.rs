use std::{collections::HashMap, sync::Arc};
use serde::{Serialize, Deserialize};
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio_tungstenite::{WebSocketStream, MaybeTlsStream};

pub type Peers = Arc<RwLock<HashMap<String, Peer>>>;

pub type Sock = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub type MessageSender = tokio::sync::mpsc::Sender<Message>;
pub type MessageReceiver = tokio::sync::mpsc::Receiver<Message>;

pub type PrintSender = tokio::sync::mpsc::Sender<String>;
pub type PrintReceiver = tokio::sync::mpsc::Receiver<String>;

pub type BlockchainPKI = HashMap<String, Identity>;

#[derive(Debug, Serialize, Deserialize)]
pub struct Identity {
    pub name: String,
    pub url: String,
    pub port: u16,
}

#[derive(Debug)]
pub struct Peer {
    pub name: String,
    pub url: String,
    pub port: u16,
    pub connection: Option<Sock>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AppNode {
    pub server: String,
    pub app: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Payload {
    pub json: Option<serde_json::Value>,
    pub bytes: Option<Vec<u8>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Message {
    pub source: AppNode,
    pub target: AppNode,
    pub payload: Payload,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ID {
    node: String,
    app_name: String,
    app_distributor: String,
    app_version: String,
}

pub enum Command {
    Message(Message),
    Quit,
    Invalid,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileSystemCommand {
    pub uri_string: String,
    pub command: FileSystemAction,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum FileSystemAction {
    Read,
    Write,
    Append,
}
