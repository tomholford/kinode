use crate::net::*;

use aes_gcm::aead::Aead;
use aes_gcm::KeyInit;
use aes_gcm_siv::{Aes256GcmSiv, Nonce};
use futures::{SinkExt, StreamExt};
use ring::signature::Ed25519KeyPair;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::{self};

/// Starts a new task and returns a sender to use for it. The task will take an open `WebSocket`
/// and maintain it as an open connection. Takes an optional initial message to send.
/// The source/target on the first valid message, whether received or sent across the connection,
/// will be considered the peer with which this connection is held.
pub async fn build_connection(
    our: Identity,
    keypair: Arc<Ed25519KeyPair>,
    initial_message: Option<PeerMessage>,
    pki: OnchainPKI,
    peers: Peers,
    mut websocket: WebSocket,
    kernel_message_tx: MessageSender,
) -> Result<UnboundedSender<(NetworkMessage, ErrorShuttle)>, SendErrorKind> {
    let with = "lol";
    let (message_tx, message_rx) = unbounded_channel::<(NetworkMessage, ErrorShuttle)>();
    tokio::spawn(maintain_connection(
        our,
        with.to_string(),
        pki,
        peers,
        websocket,
        kernel_message_tx,
        message_rx,
    ));

    return Ok(message_tx);
}

/// What `build_connection` returns a sender to. Keeps a connection alive and handles
/// sending and receiving of NetworkMessages through it.
pub async fn maintain_connection(
    our: Identity,
    with: String,
    pki: OnchainPKI,
    peers: Peers,
    websocket: WebSocket,
    kernel_message_tx: MessageSender,
    mut message_rx: UnboundedReceiver<(NetworkMessage, ErrorShuttle)>,
) {
    unimplemented!()
}

/// After a successful handshake, use information to spawn a new `peer_handler` task
/// and save a `Peer` in our peers mapping. Returns a sender to use for sending messages
/// to this peer, which will also be saved in its Peer struct.
pub async fn create_new_peer(
    our: Identity,
    new_peer_id: Identity,
    cipher: Aes256GcmSiv,
    nonce: Arc<Nonce>,
    conn_sender: UnboundedSender<(NetworkMessage, ErrorShuttle)>,
    kernel_message_tx: MessageSender,
) -> Result<Peer, ()> {
    // create

    let (message_tx, message_rx) = unbounded_channel::<(PeerMessage, ErrorShuttle)>();
    let (decrypter_tx, decrypter_rx) = unbounded_channel::<Vec<u8>>();

    let handle = tokio::spawn(peer_handler(
        our,
        new_peer_id.name.clone(),
        cipher,
        nonce,
        message_rx,
        decrypter_rx,
        conn_sender,
        kernel_message_tx,
    ));

    return Ok(Peer {
        identity: new_peer_id,
        handle,
        sender: message_tx,
        decrypter: decrypter_tx,
    })
}

/// 1. take in messages from a specific peer, decrypt them, and send to kernel
/// 2. take in messages targeted at specific peer and either:
/// - encrypt them, and send to proper connection
/// - forward them untouched along the connection
async fn peer_handler(
    our: Identity,
    who: String,
    cipher: Aes256GcmSiv,
    nonce: Arc<Nonce>,
    mut message_rx: UnboundedReceiver<(PeerMessage, ErrorShuttle)>,
    mut decrypter_rx: UnboundedReceiver<Vec<u8>>,
    socket_tx: UnboundedSender<(NetworkMessage, ErrorShuttle)>,
    kernel_message_tx: MessageSender,
) -> String {
    // println!("peer_handler\r");
    tokio::select! {
        //
        // take in messages from a specific peer, decrypt them, and send to kernel
        //
        _ = async {
            while let Some(encrypted_bytes) = decrypter_rx.recv().await {
                if let Ok(decrypted) = cipher.decrypt(&nonce, encrypted_bytes.as_ref()) {
                    if let Ok(message) = bincode::deserialize::<KernelMessage>(&decrypted) {
                        if message.source.node == who {
                            let _ = kernel_message_tx.send(message).await;
                            continue;
                        }
                    }
                }
                // println!("net: decryption error with message from {who}\r");
                break;
            }
        } => {}
        //
        // take in messages targeted at specific peer and either:
        // - encrypt them, and send to proper connection
        // - forward them untouched along the connection
        //
        _ = async {
            while let Some((message, result_tx)) = message_rx.recv().await {
                // if message is raw, we should encrypt.
                // otherwise, simply send
                match message {
                    PeerMessage::Raw(message) => {
                        if let Message::Request(ref r) = message.message {
                            println!("B #{}\r", r.ipc.as_ref().unwrap_or(&"".to_string()));
                        }
                        if let Ok(bytes) = bincode::serialize::<KernelMessage>(&message) {
                            if let Ok(encrypted) = cipher.encrypt(&nonce, bytes.as_ref()) {
                                if socket_tx.is_closed() {
                                    let _ = result_tx.unwrap().send(Err(SendErrorKind::Offline));
                                } else {
                                    let _ = socket_tx.send((
                                        NetworkMessage::Msg {
                                            from: our.name.clone(),
                                            to: who.clone(),
                                            id: message.id,
                                            contents: encrypted,
                                        },
                                        result_tx,
                                    ));
                                }
                            }
                        }
                    }
                    PeerMessage::Net(net_message) => {
                        if socket_tx.is_closed() {
                            result_tx
                                .unwrap()
                                .send(Err(SendErrorKind::Offline))
                                .unwrap();
                        } else {
                            let _ = socket_tx.send((net_message, result_tx));
                        }
                    }
                }
            }
        } => {},
    };
    return who;
}
