use std::{collections::HashMap, sync::Arc};
use serde::{Serialize, Deserialize};
use warp::{Filter};
use thiserror::Error;

use ethers::prelude::*;

pub type MessageSender = tokio::sync::mpsc::Sender<MessageStack>;
pub type MessageReceiver = tokio::sync::mpsc::Receiver<MessageStack>;

pub type PrintSender = tokio::sync::mpsc::Sender<String>;
pub type PrintReceiver = tokio::sync::mpsc::Receiver<String>;

pub type OnchainPKI = Arc<HashMap<String, Identity>>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Identity {
    pub name: String,
    pub address: H256,
    pub networking_key: String,
    pub ws_routing: Option<(String, u16)>,
    pub allowed_routers: Vec<String>,
    pub routing_for: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppNode {
    pub server: String,
    pub app: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Wire {
    pub source_ship: String,
    pub source_app:  String,
    pub target_ship: String,
    pub target_app:  String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Payload {
    pub json: Option<serde_json::Value>,
    pub bytes: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub message_type: MessageType,
    pub wire: Wire,
    pub payload: Payload,
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
    MessageTimeout
}

pub type MessageStack = Vec<Message>;

impl std::fmt::Display for Payload {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let bytes_string = match self.bytes {
            Some(_) => "Some(<elided>)",
            None => "None",
        };
        write!(
            f,
            "Payload {{ json: {:?}, bytes: {} }}",
            self.json,
            bytes_string,
        )
    }
}

impl std::fmt::Display for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "Message {{ message_type: {:?}, wire: {:?}, payload: {} }}",
            self.message_type,
            self.wire,
            self.payload,
        )
    }
}

pub enum Command {
    StartOfMessageStack(MessageStack),
    Quit,
    Invalid,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum FileSystemRequest {
    Read(String),
    Write(String),
    Append(String),
    AlterReadPermissions(Vec<String>)
}

// http_server commands
//
#[derive(Debug, Serialize, Deserialize)]
pub enum HttpAction {
    HttpConnect(HttpConnect),
    HttpResponse(HttpResponse),
}

// all incoming requests to http://<url>/<path> will get sent to app
#[derive(Debug, Serialize, Deserialize)]
pub struct HttpConnect {
    pub path: String,
    pub app: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HttpResponse {
    pub id: String, // TODO uuid?
    pub status: u16,
    pub headers: String, // TODO HashMap<String, String>?
    pub body: String, // TODO type (probably bytes?)
}

