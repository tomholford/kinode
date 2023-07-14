use std::{collections::HashMap, sync::Arc};
use serde::{Serialize, Deserialize};
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio_tungstenite::{WebSocketStream, MaybeTlsStream};

pub type Peers = Arc<RwLock<HashMap<String, Peer>>>;

pub type Sock = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub type MessageSender = tokio::sync::mpsc::Sender<MessageStack>;
pub type MessageReceiver = tokio::sync::mpsc::Receiver<MessageStack>;

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
pub struct Wire {
    pub source_ship: String,
    pub source_app:  String,
    pub target_ship: String,
    pub target_app:  String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Payload {
    pub json: Option<serde_json::Value>,
    pub bytes: Option<Vec<u8>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Message {
    pub message_type: MessageType,
    pub wire: Wire,
    pub payload: Payload,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum MessageType {
    Request(bool),
    Response,
}

impl Clone for MessageType {
    fn clone(&self) -> MessageType {
        match self {
            MessageType::Request(is_expecting_response) => {
                MessageType::Request(is_expecting_response.clone())
            },
            MessageType::Response => MessageType::Response,
        }
    }
}

impl Clone for Wire {
    fn clone(&self) -> Wire {
        Wire {
            source_ship: self.source_ship.clone(),
            source_app: self.source_app.clone(),
            target_ship: self.target_ship.clone(),
            target_app: self.target_app.clone(),
        }
    }
}

impl Clone for Payload {
    fn clone(&self) -> Payload {
        Payload {
            json: self.json.clone(),
            bytes: self.bytes.clone(),
        }
    }
}

impl Clone for Message {
    fn clone(&self) -> Message {
        Message {
            message_type: self.message_type.clone(),
            wire: self.wire.clone(),
            payload: self.payload.clone(),
        }
    }
}

pub type MessageStack = Vec<Message>;

#[derive(Debug, Serialize, Deserialize)]
pub struct ID {
    node: String,
    app_name: String,
    app_distributor: String,
    app_version: String,
}

pub enum Command {
    StartOfMessageStack(MessageStack),
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
