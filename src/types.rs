use std::collections::HashMap;
use serde::{Serialize, Deserialize};
use warp::{Filter, Rejection, Reply, reply::html};

use ethers::prelude::*;

pub type MessageSender = tokio::sync::mpsc::Sender<MessageStack>;
pub type MessageReceiver = tokio::sync::mpsc::Receiver<MessageStack>;

pub type PrintSender = tokio::sync::mpsc::Sender<String>;
pub type PrintReceiver = tokio::sync::mpsc::Receiver<String>;

pub type OnchainPKI = HashMap<String, Identity>;

#[derive(Debug, Serialize, Deserialize)]
pub struct Identity {
    pub name: String,
    pub address: H256,
    pub ws_url: String,
    pub ws_port: u16,
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

// #[derive(Debug, Serialize, Deserialize)]
// pub struct ID {
//     node: String,
//     app_name: String,
//     app_distributor: String,
//     app_version: String,
// }

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

// http types
// TODO this is way too basic
#[derive(Serialize, Deserialize)]
pub enum DynamicRoute {
    Hello,
    Greet(String),
    // Add more variants as needed for different routes
}

impl Reply for DynamicRoute {
    fn into_response(self) -> warp::reply::Response {
        match self {
            DynamicRoute::Hello => html("<h1>Hello, World!</h1>").into_response(),
            DynamicRoute::Greet(name) => html(format!("<h1>Hello, {}!</h1>", name)).into_response(),
        }
    }
}

// TODO needs enum or struct for action type (URL+Contents)
// 