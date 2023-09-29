use crate::net::*;
use aes_gcm::aead::Aead;
use aes_gcm::KeyInit;
use aes_gcm_siv::{Aes256GcmSiv, Nonce};
use elliptic_curve::ecdh::SharedSecret;
use futures::{SinkExt, StreamExt};
use ring::signature::Ed25519KeyPair;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::{self};

pub async fn build_connection(
    our: Identity,
    keypair: Arc<Ed25519KeyPair>,
    pki: OnchainPKI,
    keys: PeerKeys,
    peers: Peers,
    websocket: WebSocket,
    kernel_message_tx: MessageSender,
    with: Option<String>,
) -> (
    UnboundedSender<(NetworkMessage, Option<ErrorShuttle>)>,
    JoinHandle<Option<String>>,
) {
    println!("building new connection\r");
    // create a sender and receiver to pass messages from peers to this connection.
    // when we receive a message from a new peer, we can set their sender to this.
    let (message_tx, message_rx) = unbounded_channel::<(NetworkMessage, Option<ErrorShuttle>)>();
    let handle = tokio::spawn(maintain_connection(
        our,
        with,
        keypair,
        pki,
        keys,
        peers,
        websocket,
        message_tx.clone(),
        message_rx,
        kernel_message_tx,
    ));
    return (message_tx, handle);
}

/// Keeps a connection alive and handles sending and receiving of NetworkMessages through it.
/// TODO add a keepalive PING/PONG system
pub async fn maintain_connection(
    our: Identity,
    with: Option<String>,
    keypair: Arc<Ed25519KeyPair>,
    pki: OnchainPKI,
    keys: PeerKeys,
    peers: Peers,
    websocket: WebSocket,
    message_tx: UnboundedSender<(NetworkMessage, Option<ErrorShuttle>)>,
    mut message_rx: UnboundedReceiver<(NetworkMessage, Option<ErrorShuttle>)>,
    kernel_message_tx: MessageSender,
) -> Option<String> {
    let conn_id: u64 = rand::random();
    println!("maintaining connection {conn_id}\r");

    let message_max_size = websocket.get_config().max_frame_size.unwrap();

    // accept messages on the websocket in one task, and send messages in another
    let (mut write_stream, mut read_stream) = websocket.split();

    // manage outstanding ACKs from messages sent over the connection
    // TODO replace with more performant data structure
    let ack_map = Arc::new(RwLock::new(HashMap::<u64, ErrorShuttle>::new()));
    let sender_ack_map = ack_map.clone();

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
                    let Some(result_tx) = ack_map.write().await.remove(&id) else {
                        println!("conn {conn_id}: got unexpected Ack {id}\r");
                        continue;
                    };
                    println!("conn {conn_id}: got Ack {id}\r");
                    let _ = result_tx.send(Ok(None));
                    continue;
                }
                NetworkMessage::Nack(id) => {
                    let Some(result_tx) = ack_map.write().await.remove(&id) else {
                        println!("net: got unexpected Nack\r");
                        continue;
                    };
                    let _ = result_tx.send(Err(SendErrorKind::Offline));
                    continue;
                }
                NetworkMessage::Msg {
                    ref id,
                    ref from,
                    ref to,
                    ref contents,
                } => {
                    println!("conn {conn_id}: handling msg {id}\r");
                    // if the message is *directed to us*, try to handle with the
                    // matching peer handler "decrypter".
                    //
                    if to == &our.name {
                        let (result_tx, result_rx) = oneshot::channel::<MessageResult>();
                        // if we have the peer, send the message to them.
                        if let Some(peer) = peers.read().await.get(from) {
                            let _ = peer.decrypter.send((contents.to_owned(), result_tx));
                            let message_tx = message_tx.clone();
                            tokio::spawn(async move {
                                match timeout(TIMEOUT, result_rx).await {
                                    Ok(Ok(Ok(Some(network_message)))) => {
                                        message_tx.send((network_message, None)).unwrap();
                                    }
                                    _ => {}
                                }
                            });
                            continue;
                        }
                        // if we don't have the peer, see if we have the keys to create them.
                        // if we don't have their keys, throw a nack.
                        if let Some((peer_id, secret, nonce)) = keys.read().await.get(from) {
                            let new_peer = create_new_peer(
                                &our,
                                &peer_id,
                                secret,
                                &nonce,
                                message_tx.clone(),
                                kernel_message_tx.clone(),
                            );
                            let _ = new_peer.decrypter.send((contents.to_owned(), result_tx));
                            peers.write().await.insert(peer_id.name.clone(), new_peer);
                            let message_tx = message_tx.clone();
                            tokio::spawn(async move {
                                match timeout(TIMEOUT, result_rx).await {
                                    Ok(Ok(Ok(Some(network_message)))) => {
                                        message_tx.send((network_message, None)).unwrap();
                                    }
                                    _ => {}
                                }
                            });
                            continue;
                        }
                        println!("net: nacking message {id}\r");
                        message_tx.send((NetworkMessage::Nack(*id), None)).unwrap();
                    } else {
                        // if the message is *directed to someone else*, try to handle
                        // with the matching peer handler "sender".
                        //
                        let (result_tx, result_rx) = oneshot::channel::<MessageResult>();
                        if let Some(peer) = peers.read().await.get(to) {
                            let id = id.clone();
                            let message_tx = message_tx.clone();
                            let _ = peer.sender.send((PeerMessage::Net(net_message), result_tx));
                            tokio::spawn(async move {
                                match timeout(TIMEOUT, result_rx).await {
                                    Ok(Ok(Ok(_))) => {
                                        // println!("net_inner: got Ack {id}\r");
                                        message_tx.send((NetworkMessage::Ack(id), None)).unwrap();
                                    }
                                    _ => {
                                        message_tx.send((NetworkMessage::Nack(id), None)).unwrap();
                                    }
                                }
                            });
                            continue;
                        }
                        // if we don't have the peer, throw a nack.
                        // println!("net: nacking message with id {id}\r");
                        message_tx.send((NetworkMessage::Nack(*id), None)).unwrap();
                    }
                }
                NetworkMessage::Handshake(ref handshake) => {
                    // when we get a handshake, if we are the target,
                    // 1. verify it against the PKI
                    // 2. send a response handshakeAck
                    // 3. create a Peer and save, replacing old one if it existed
                    if handshake.target == our.name {
                        let Some(peer_id) = pki.read().await.get(&handshake.from).cloned() else {
                            println!(
                                "net: failed handshake with unknown node {}\r",
                                handshake.from
                            );
                            continue;
                        };
                        let their_ephemeral_pk = match validate_handshake(&handshake, &peer_id) {
                            Ok(pk) => pk,
                            Err(e) => {
                                println!("net: invalid handshake from {}: {}\r", handshake.from, e);
                                continue;
                            }
                        };
                        // use the nonce from the initiatory handshake, always
                        let nonce = *Nonce::from_slice(&handshake.nonce);
                        let (secret, handshake) = make_secret_and_handshake(
                            &our,
                            keypair.clone(),
                            &handshake.from,
                            Some(handshake.id),
                        );
                        message_tx
                            .send((NetworkMessage::HandshakeAck(handshake), None))
                            .unwrap();
                        let secret = Arc::new(secret.diffie_hellman(&their_ephemeral_pk));
                        // save the handshake to our Keys map
                        keys.write().await.insert(
                            peer_id.name.clone(),
                            (peer_id.clone(), secret.clone(), nonce),
                        );
                        let new_peer = create_new_peer(
                            &our,
                            &peer_id,
                            &secret,
                            &nonce,
                            message_tx.clone(),
                            kernel_message_tx.clone(),
                        );
                        peers.write().await.insert(peer_id.name.clone(), new_peer);
                    } else {
                        // if we are NOT the target,
                        // try to send it to the matching peer handler "sender"
                        // if we don't have the peer, throw a nack.
                        let (result_tx, result_rx) = oneshot::channel::<MessageResult>();
                        if let Some(peer) = peers.read().await.get(&handshake.target) {
                            let id = handshake.id.clone();
                            let ack_map = ack_map.clone();
                            let message_tx = message_tx.clone();
                            let _ = peer.sender.send((PeerMessage::Net(net_message), result_tx));
                            tokio::spawn(async move {
                                match timeout(TIMEOUT, result_rx).await {
                                    Ok(Ok(Ok(Some(NetworkMessage::HandshakeAck(h))))) => {
                                        ack_map.write().await.remove(&h.id);
                                        message_tx
                                            .send((NetworkMessage::HandshakeAck(h), None))
                                            .unwrap();
                                    }
                                    _ => {
                                        message_tx.send((NetworkMessage::Nack(id), None)).unwrap();
                                    }
                                }
                            });
                            continue;
                        }
                        // if we don't have the peer, throw a nack.
                        // println!("net: nacking handshake with id {}\r", handshake.id);
                        message_tx
                            .send((NetworkMessage::Nack(handshake.id), None))
                            .unwrap();
                    }
                }
                NetworkMessage::HandshakeAck(ref handshake) => {
                    let Some(result_tx) = ack_map.write().await.remove(&handshake.id) else {
                        continue;
                    };
                    let _ = result_tx.send(Ok(Some(net_message)));
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
                    if bytes.len() > message_max_size {
                        println!("net: message too large\r");
                        let _ = result_tx.unwrap().send(Err(SendErrorKind::Timeout));
                        continue;
                    }
                    match &message {
                        NetworkMessage::Msg { id, .. } => {
                            println!("conn {conn_id}: piping msg {id}\r");
                            sender_ack_map.write().await.insert(*id, result_tx.unwrap());
                        }
                        NetworkMessage::Handshake(h) => {
                            sender_ack_map.write().await.insert(h.id, result_tx.unwrap());
                        }
                        _ => {}
                    }
                    match write_stream.send(tungstenite::Message::Binary(bytes)).await {
                        Ok(()) => {}
                        Err(e) => {
                            println!("net: send error: {:?}\r", e);
                            let id = match &message {
                                NetworkMessage::Msg { id, .. } => id,
                                NetworkMessage::Handshake(h) => &h.id,
                                _ => continue,
                            };
                            let Some(result_tx) = sender_ack_map.write().await.remove(&id) else {
                                continue;
                            };
                            // TODO learn how to handle other non-fatal websocket errors.
                            match e {
                                tungstenite::error::Error::Capacity(_)
                                | tungstenite::Error::Io(_) => {
                                    let _ = result_tx.send(Err(SendErrorKind::Timeout));
                                }
                                _ => {
                                    let _ = result_tx.send(Err(SendErrorKind::Offline));
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
    return with;
}

/// After a successful handshake, use information to spawn a new `peer_handler` task
/// and save a `Peer` in our peers mapping. Returns a sender to use for sending messages
/// to this peer, which will also be saved in its Peer struct.
pub fn create_new_peer(
    our: &Identity,
    new_peer_id: &Identity,
    secret: &SharedSecret<Secp256k1>,
    nonce: &Nonce,
    conn_sender: UnboundedSender<(NetworkMessage, Option<ErrorShuttle>)>,
    kernel_message_tx: MessageSender,
) -> Peer {
    let mut key = [0u8; 32];
    secret
        .extract::<sha2::Sha256>(None)
        .expand(&[], &mut key)
        .unwrap();
    let cipher = Aes256GcmSiv::new(generic_array::GenericArray::from_slice(&key));
    let (message_tx, message_rx) = unbounded_channel::<(PeerMessage, ErrorShuttle)>();
    let (decrypter_tx, decrypter_rx) = unbounded_channel::<(Vec<u8>, ErrorShuttle)>();
    let handle = tokio::spawn(peer_handler(
        our.clone(),
        new_peer_id.name.clone(),
        cipher,
        nonce.clone(),
        message_rx,
        decrypter_rx,
        conn_sender.clone(),
        kernel_message_tx,
    ));
    return Peer {
        identity: new_peer_id.clone(),
        handle,
        sender: message_tx,
        decrypter: decrypter_tx,
        socket_tx: conn_sender,
    };
}

/// 1. take in messages from a specific peer, decrypt them, and send to kernel
/// 2. take in messages targeted at specific peer and either:
/// - encrypt them, and send to proper connection
/// - forward them untouched along the connection
async fn peer_handler(
    our: Identity,
    who: String,
    cipher: Aes256GcmSiv,
    nonce: Nonce,
    mut message_rx: UnboundedReceiver<(PeerMessage, ErrorShuttle)>,
    mut decrypter_rx: UnboundedReceiver<(Vec<u8>, ErrorShuttle)>,
    socket_tx: UnboundedSender<(NetworkMessage, Option<ErrorShuttle>)>,
    kernel_message_tx: MessageSender,
) -> String {
    // println!("peer_handler\r");
    tokio::select! {
        //
        // take in messages from a specific peer, decrypt them, and send to kernel
        //
        _ = async {
            while let Some((encrypted_bytes, result_tx)) = decrypter_rx.recv().await {
                if let Ok(decrypted) = cipher.decrypt(&nonce, encrypted_bytes.as_ref()) {
                    if let Ok(message) = bincode::deserialize::<KernelMessage>(&decrypted) {
                        if message.source.node == who {
                            println!("net: got peer message {}, acking\r", message.id);
                            let _ = result_tx.send(Ok(Some(NetworkMessage::Ack(message.id))));
                            let _ = kernel_message_tx.send(message).await;
                            continue;
                        }
                        println!("net: got message 'from' wrong person, nacking\r");
                        let _ = result_tx.send(Ok(Some(NetworkMessage::Nack(message.id))));
                        break;
                    }
                    println!("net: failed to deserialize message from {}\r", who);
                    break;
                }
                println!("net: failed to decrypt message from {}\r", who);
                break;
            }
        } => { println!("net: lost peer {who}\r"); }
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
                        if let Ok(bytes) = bincode::serialize::<KernelMessage>(&message) {
                            if let Ok(encrypted) = cipher.encrypt(&nonce, bytes.as_ref()) {
                                match socket_tx.send((
                                    NetworkMessage::Msg {
                                        from: our.name.clone(),
                                        to: who.clone(),
                                        id: message.id,
                                        contents: encrypted,
                                    },
                                    Some(result_tx),
                                )) {
                                    Ok(()) => tokio::task::yield_now().await,
                                    Err(tokio::sync::mpsc::error::SendError((_, result_tx))) => {
                                        println!("net: lost socket with {who}\r");
                                        let _ = result_tx.unwrap().send(Err(SendErrorKind::Offline));
                                        break;
                                    },
                                }
                            }
                        }
                    }
                    PeerMessage::Net(net_message) => {
                        match socket_tx.send((net_message, Some(result_tx))) {
                            Ok(()) => continue,
                            Err(tokio::sync::mpsc::error::SendError((_, result_tx))) => {
                                println!("net: lost *forwarding* socket with {who}\r");
                                let _ = result_tx.unwrap().send(Err(SendErrorKind::Offline));
                                break;
                            },
                        }
                    }
                }
            }
        } => { println!("net: deleting peer {who}\r"); },
    };
    return who;
}
