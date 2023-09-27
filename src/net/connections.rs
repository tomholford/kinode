use crate::net::*;
use aes_gcm::aead::Aead;
use aes_gcm::KeyInit;
use aes_gcm_siv::{Aes256GcmSiv, Nonce};
use elliptic_curve::ecdh::SharedSecret;
use futures::{SinkExt, StreamExt};
use ring::signature::Ed25519KeyPair;
use std::fmt::write;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::error::CapacityError;
use tokio_tungstenite::tungstenite::{self};

/// Keeps a connection alive and handles sending and receiving of NetworkMessages through it.
pub async fn maintain_connection(
    our: Identity,
    keypair: Arc<Ed25519KeyPair>,
    pki: OnchainPKI,
    keys: PeerKeys,
    peers: Peers,
    websocket: WebSocket,
) {
    // create a sender and receiver to pass messages from peers to this connection.
    // when we receive a message from a new peer, we can set their sender to this.
    let (message_tx, mut message_rx) =
        unbounded_channel::<(NetworkMessage, Option<ErrorShuttle>)>();

    // accept messages on the websocket in one task, and send messages in another
    let (mut write_stream, mut read_stream) = websocket.split();

    // receive messages from over the websocket and route them to the correct peer handler,
    // or create it, if necessary.
    let ws_receiver = tokio::spawn(async move {
        while let Some(Ok(tungstenite::Message::Binary(bin))) = read_stream.next().await {
            // TODO use a language-netural serialization format here!
            let Ok(net_message) = bincode::deserialize::<NetworkMessage>(&bin) else {
                continue;
            };
            match net_message {
                NetworkMessage::Ack(id) => {
                    // TODO handle with ack-map
                }
                NetworkMessage::Nack(id) => {
                    // TODO handle with ack-map
                }
                NetworkMessage::Msg {
                    id,
                    from,
                    to,
                    contents,
                } => {
                    // if the message is *directed to us*, try to handle with the
                    // matching peer handler "decrypter".
                    //
                    // if we don't have the peer, see if we have the keys to create them.
                    // if we don't have their keys, throw a nack.

                    // if the message is *directed to someone else*, try to handle
                    // with the matching peer handler "sender".
                    //
                    // if we don't have the peer, throw a nack.
                    unimplemented!();
                }
                NetworkMessage::Handshake(handshake) => {
                    // when we get a handshake, if we are the target,
                    // 1. verify it against the PKI
                    // 2. send a response handshakeAck
                    // 3. create a Peer and save, replacing old one if it existed

                    // if we are NOT the target,
                    // try to send it to the matching peer handler "sender"
                    // if we don't have the peer, throw a nack.
                    unimplemented!();
                }
                NetworkMessage::HandshakeAck(handshake) => {
                    // when we get a handshakeAck, if we are the target,
                    // 1. verify it against the PKI
                    // 2. create a Peer and save, replacing old one if it existed

                    // if we are NOT the target,
                    // try to send it to the matching peer handler "sender"
                    // if we don't have the peer, throw a nack.
                    unimplemented!();
                }
            }
        }
    });

    tokio::select! {
        _ = ws_receiver => {
            println!("ws_receiver died\r");
        },
        // receive messages we would like to send to peers along this connection
        // and send them to the websocket
        _ = async {
            while let Some((message, result_tx)) = message_rx.recv().await {
                // TODO use a language-netural serialization format here!
                if let Ok(bytes) = bincode::serialize::<NetworkMessage>(&message) {
                    match write_stream.send(tungstenite::Message::Binary(bytes)).await {
                        Ok(()) => continue,
                        Err(e) => {
                            println!("net: send error: {:?}\r", e);
                            // TODO learn how to handle other non-fatal websocket errors.
                            match e {
                                tungstenite::error::Error::Capacity(_) => {
                                    match result_tx {
                                        Some(result_tx) => {
                                            let _ = result_tx.send(Err(SendErrorKind::Timeout));
                                            continue;
                                        }
                                        None => continue,
                                    }
                                }
                                _ => {
                                    match result_tx {
                                        Some(result_tx) => {
                                            let _ = result_tx.send(Err(SendErrorKind::Offline));
                                            break;
                                        }
                                        None => break,
                                    }
                                }
                            }
                        }
                    }
                }
            }
        } => {
            println!("ws_sender died\r");
        },
    };
}

/// After a successful handshake, use information to spawn a new `peer_handler` task
/// and save a `Peer` in our peers mapping. Returns a sender to use for sending messages
/// to this peer, which will also be saved in its Peer struct.
pub async fn create_new_peer(
    our: Identity,
    new_peer_id: Identity,
    secret: SharedSecret<Secp256k1>,
    nonce: Arc<Nonce>,
    conn_sender: UnboundedSender<(NetworkMessage, Option<ErrorShuttle>)>,
    kernel_message_tx: MessageSender,
) -> Result<Peer, ()> {
    let mut key = [0u8; 32];
    secret.extract::<sha2::Sha256>(None).expand(&[], &mut key).unwrap();
    let cipher = Aes256GcmSiv::new(generic_array::GenericArray::from_slice(&key));

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
    });
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
    socket_tx: UnboundedSender<(NetworkMessage, Option<ErrorShuttle>)>,
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
                break;
            }
        } => { println!("net: decryption error with message from {who}\r"); }
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
                                    let _ = result_tx.send(Err(SendErrorKind::Offline));
                                } else {
                                    match socket_tx.send((
                                        NetworkMessage::Msg {
                                            from: our.name.clone(),
                                            to: who.clone(),
                                            id: message.id,
                                            contents: encrypted,
                                        },
                                        Some(result_tx),
                                    )) {
                                        Ok(()) => {
                                            // TODO: save ack here in a map with the result thingy
                                        },
                                        Err(tokio::sync::mpsc::error::SendError((_, result_tx))) => {
                                            let _ = result_tx.unwrap().send(Err(SendErrorKind::Offline));
                                        },
                                    }
                                }
                            }
                        }
                    }
                    PeerMessage::Net(net_message) => {
                        match socket_tx.send((net_message, Some(result_tx))) {
                            Ok(()) => continue,
                            Err(tokio::sync::mpsc::error::SendError((_, result_tx))) => {
                                let _ = result_tx.unwrap().send(Err(SendErrorKind::Offline));
                                break;
                            },
                        }
                    }
                }
            }
        } => { println!("net: send error with message to {who}\r"); },
    };
    return who;
}
