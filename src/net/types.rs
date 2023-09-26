use crate::types::*;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

pub type Peers = Arc<RwLock<HashMap<String, Peer>>>;
pub type WebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
pub type MessageResult = Result<Option<NetworkMessage>, SendErrorKind>;
pub type ErrorShuttle = Option<oneshot::Sender<MessageResult>>;

// stored in mapping by their username
pub struct Peer {
    pub networking_address: String,
    pub handle: tokio::task::JoinHandle<()>,
    pub sender: mpsc::UnboundedSender<(NetworkMessage, ErrorShuttle)>,
    pub handler: mpsc::UnboundedSender<Vec<u8>>,
}

/// parsed from Binary websocket message on an Indirect route
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NetworkMessage {
    Ack(u64),
    Nack(u64),
    Msg {
        from: String,
        to: String,
        id: u64,
        contents: Vec<u8>,
    },
    Raw(KernelMessage),
    Handshake(Handshake),
    HandshakeAck(Handshake),
    Keepalive,
}

/// contains identity and encryption keys, used in initial handshake.
/// parsed from Text websocket message
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Handshake {
    pub from: String,
    pub target: String,
    pub id_signature: Vec<u8>,
    pub ephemeral_public_key: Vec<u8>,
    pub ephemeral_public_key_signature: Vec<u8>,
    pub nonce: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NetActions {
    QnsUpdate(QnsUpdate),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QnsUpdate {
    pub name: String, // actual username / domain name
    pub owner: String,
    pub node: String, // hex namehash of node
    pub public_key: String,
    pub ip: String,
    pub port: u16,
    pub routers: Vec<String>,
}
