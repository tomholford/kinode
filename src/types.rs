use std::collections::HashMap;
use serde::{Serialize, Deserialize};

use ethers::prelude::*;

pub type MessageSender = tokio::sync::mpsc::Sender<Message>;
pub type MessageReceiver = tokio::sync::mpsc::Receiver<Message>;

pub type PrintSender = tokio::sync::mpsc::Sender<String>;
pub type PrintReceiver = tokio::sync::mpsc::Receiver<String>;

pub type OnchainPKI = HashMap<String, Identity>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Identity {
    pub name: String,
    pub address: H256,
    pub networking_key: String,
    pub ws_url: String,
    pub ws_port: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppNode {
    pub server: String,
    pub app: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Payload {
    pub json: Option<serde_json::Value>,
    pub bytes: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub source: AppNode,
    pub target: AppNode,
    pub payload: Payload,
}

// #[derive(Debug, Serialize, Deserialize)]
// pub struct ID {
//     node: String,
//     app_name: String,
//     app_distributor: String,
//     app_version: String,
// }

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
