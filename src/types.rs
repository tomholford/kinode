use std::{collections::HashMap, sync::Arc};
use serde::{Serialize, Deserialize};
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio_tungstenite::{WebSocketStream, MaybeTlsStream};

pub type Peers = Arc<RwLock<HashMap<String, Peer>>>;

pub type Sock = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub type CardSender = tokio::sync::mpsc::Sender<Card>;
pub type CardReceiver = tokio::sync::mpsc::Receiver<Card>;

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
pub struct Card {
    pub source: String,
    pub target: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ID {
    node: String,
    app_name: String,
    app_distributor: String,
    app_version: String,
}

pub enum Command {
    Card(Card),
    Quit,
    Invalid,
}
