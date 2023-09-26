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

/// returns JoinHandle to active_peer if created
pub async fn build_connection(
    our: Identity,
    keypair: Arc<Ed25519KeyPair>,
    target: Option<Identity>,
    initial_message: Option<(NetworkMessage, ErrorShuttle)>,
    pki: OnchainPKI,
    peers: Peers,
    mut websocket: WebSocket,
    kernel_message_tx: MessageSender,
) -> Result<JoinHandle<String>, SendErrorKind> {
    // println!("build_connection\r");
    let (cipher, nonce, their_id) = match target {
        Some(target) => {
            // we have target, we are initiating
            let (ephemeral_secret, our_handshake) =
                make_secret_and_handshake(&our, keypair.clone(), &target.name);
            let _ = websocket
                .send(tungstenite::Message::Text(
                    serde_json::to_string(&our_handshake).unwrap_or("".into()),
                ))
                .await;
            let their_handshake = match get_handshake(&mut websocket).await {
                Ok(h) => h,
                Err(_) => return Err(SendErrorKind::Offline),
            };
            let (their_ephemeral_pk, nonce) =
                match validate_handshake(&their_handshake, &target, our_handshake.nonce.clone()) {
                    Ok(v) => v,
                    Err(e) => {
                        println!("handshake validation failed: {}\r", e);
                        return Err(SendErrorKind::Offline);
                    }
                };
            let encryption_key = ephemeral_secret.diffie_hellman(&their_ephemeral_pk);
            let cipher = Aes256GcmSiv::new(&encryption_key.raw_secret_bytes());
            (cipher, nonce, target)
        }
        None => {
            // no target yet, wait for handshake to come in, then reply
            let their_handshake = match get_handshake(&mut websocket).await {
                Ok(h) => h,
                Err(_) => return Err(SendErrorKind::Offline),
            };
            let their_id = match pki.read().await.get(&their_handshake.from) {
                Some(id) => id.clone(),
                None => return Err(SendErrorKind::Offline),
            };
            let (their_ephemeral_pk, nonce) = match validate_handshake(
                &their_handshake,
                &their_id,
                their_handshake.nonce.clone(),
            ) {
                Ok(v) => v,
                Err(e) => {
                    println!("handshake validation failed: {}\r", e);
                    return Err(SendErrorKind::Offline);
                }
            };
            let (ephemeral_secret, our_handshake) =
                make_secret_and_handshake(&our, keypair.clone(), &their_id.name);
            let _ = websocket
                .send(tungstenite::Message::Text(
                    serde_json::to_string(&our_handshake).unwrap_or("".into()),
                ))
                .await;
            let encryption_key = ephemeral_secret.diffie_hellman(&their_ephemeral_pk);
            let cipher = Aes256GcmSiv::new(&encryption_key.raw_secret_bytes());
            (cipher, nonce, their_id)
        }
    };

    // sender -> socket, socket -> handler
    let (sender_tx, sender_rx) = unbounded_channel::<(NetworkMessage, ErrorShuttle)>();
    let (socket_tx, socket_rx) = unbounded_channel::<(NetworkMessage, ErrorShuttle)>();
    let (handler_tx, handler_rx) = unbounded_channel::<Vec<u8>>();

    let peer_handler = tokio::spawn(peer_handler(
        our.clone(),
        their_id.name.clone(),
        cipher,
        nonce,
        sender_rx,
        handler_rx,
        socket_tx.clone(),
        kernel_message_tx.clone(),
    ));

    let peer = Peer {
        networking_address: their_id.networking_key,
        handle: peer_handler,
        sender: sender_tx.clone(),
        handler: handler_tx.clone(),
    };

    let t_our = our.clone();
    let t_name = their_id.name.clone();
    let t_keypair = keypair.clone();
    let t_pki = pki.clone();
    let t_peers = peers.clone();
    let t_kernel_message_tx = kernel_message_tx.clone();
    let connection_handler = std::thread::spawn(move || {
        maintain_connection(
            t_our,
            t_name,
            t_keypair,
            t_pki,
            t_peers,
            socket_tx,
            socket_rx,
            websocket,
            t_kernel_message_tx,
        )
    });
    // connection is now ready to write to
    let active_peer = tokio::spawn(active_peer(
        their_id.name.clone(),
        sender_tx.clone(),
        connection_handler,
    ));
    if let Some(old_peer) = peers.write().await.insert(their_id.name.clone(), peer) {
        let _ = old_peer.handle.abort();
    }
    if let Some(to_send) = initial_message {
        let _ = sender_tx.send(to_send);
    }
    Ok(active_peer)
}

/// returns name of peer when it dies
async fn active_peer(
    who: String,
    sender: UnboundedSender<(NetworkMessage, ErrorShuttle)>,
    connection_handler: std::thread::JoinHandle<impl futures::Future<Output = ()>>,
) -> String {
    // println!("active_peer\r");
    let keepalive = tokio::spawn(async move {
        loop {
            // println!("doing a keepalive\r");
            let (result_tx, result_rx) = oneshot::channel::<MessageResult>();
            let _ = sender.send((NetworkMessage::Keepalive, Some(result_tx)));
            match result_rx.await {
                Ok(Ok(Some(NetworkMessage::Ack(_)))) => {
                    // println!("alive was kept\r");
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                }
                _ => break,
            }
        }
    });
    tokio::select! {
        _ = connection_handler.join().unwrap() => {},
        _ = keepalive => {},
    }
    return who;
}

async fn ack_waiter(mut ack_rx: UnboundedReceiver<NetworkMessage>, shuttle: ErrorShuttle) {
    if shuttle.is_none() {
        return;
    }
    match timeout(TIMEOUT, ack_rx.recv()).await {
        Ok(Some(NetworkMessage::Nack(_))) => {
            let _ = shuttle.unwrap().send(Err(SendErrorKind::Offline));
        }
        Ok(Some(msg)) => {
            let _ = shuttle.unwrap().send(Ok(Some(msg)));
        }
        Ok(None) => {
            let _ = shuttle.unwrap().send(Ok(None));
        }
        _ => {
            let _ = shuttle.unwrap().send(Err(SendErrorKind::Timeout));
        }
    }
}

/// send and receive messages on an existing websocket.
/// if this breaks it's a timeout
async fn maintain_connection(
    our: Identity,
    with: String, // name of direct peer this socket is with
    keypair: Arc<Ed25519KeyPair>,
    pki: OnchainPKI,
    peers: Peers,
    self_tx: UnboundedSender<(NetworkMessage, ErrorShuttle)>,
    mut message_rx: UnboundedReceiver<(NetworkMessage, ErrorShuttle)>,
    websocket: WebSocket,
    kernel_message_tx: MessageSender,
) {
    // println!("maintain_connection\r");
    let message_max_size = websocket.get_config().max_frame_size.unwrap();
    let (mut write_stream, mut read_stream) = websocket.split();
    let mut outstanding_acks = VecDeque::<UnboundedSender<NetworkMessage>>::new();

    loop {
        tokio::select! {
            Some((message, result_tx)) = message_rx.recv() => {
                if result_tx.is_some() { println!("C\r"); }
                // can use a buffer here but doesn't seem to affect performance
                let bytes = bincode::serialize(&message).unwrap();
                // println!("send of size: {:.2}mb\r", bytes.len() as f64 / 1_048_576.0);
                if bytes.len() > message_max_size {
                    // println!("message too large\r");
                    let _ = match result_tx {
                        Some(tx) => tx.send(Err(SendErrorKind::Timeout)),
                        None => Ok(()),
                    };
                    continue;
                }
                let _ = write_stream.send(tungstenite::Message::Binary(
                    bytes
                )).await;
                // println!("..sent\r");

                match message {
                    NetworkMessage::Raw(_)
                    | NetworkMessage::Ack(_)
                    | NetworkMessage::Nack(_)
                    | NetworkMessage::HandshakeAck { .. } => continue,
                    NetworkMessage::Keepalive => {
                        let (ack_tx, ack_rx) = unbounded_channel::<NetworkMessage>();
                        // keepalives get *first priority* in acknowledgement!
                        outstanding_acks.push_back(ack_tx);
                        tokio::spawn(ack_waiter(
                            ack_rx,
                            result_tx,
                        ));
                    }
                    NetworkMessage::Handshake { .. }
                    | NetworkMessage::Msg { .. } => {
                        let (ack_tx, ack_rx) = unbounded_channel::<NetworkMessage>();
                        outstanding_acks.push_front(ack_tx);
                        tokio::spawn(ack_waiter(
                            ack_rx,
                            result_tx,
                        ));
                    }
                }
            },
            Some(incoming) = read_stream.next() => {
                let Ok(tungstenite::Message::Binary(bin)) = incoming else {
                    // println!("got a ??\r");
                    // println!("{:?}\r", incoming);
                    break
                };
                let Ok(msg) = bincode::deserialize::<NetworkMessage>(&bin) else { break };
                match msg {
                    NetworkMessage::Raw(_) => continue,
                    NetworkMessage::Ack(_)
                    | NetworkMessage::HandshakeAck { .. }
                    | NetworkMessage::Nack(_) => {
                        // println!("got ack\r");
                        if let Some(sender) = outstanding_acks.pop_back() {
                            let _ = sender.send(msg);
                        }
                    }
                    NetworkMessage::Keepalive => {
                        let _ = self_tx.send((NetworkMessage::Ack(0), None));
                    }
                    NetworkMessage::Handshake(handshake) => {
                        // if we get an initiatory handshake directed to us,
                        // respond with our own, and spawn an active_routed_peer!
                        let our = our.clone();
                        let pki = pki.clone();
                        let keypair = keypair.clone();
                        let self_tx = self_tx.clone();
                        let kernel_message_tx = kernel_message_tx.clone();
                        let peers = peers.clone();
                        let their_handshake = handshake.clone();
                        tokio::spawn(async move {
                            if their_handshake.target == our.name {
                                // println!("got indirect handshake from {} for us\r", their_handshake.from);
                                let their_id = match pki.read().await.get(&their_handshake.from) {
                                    Some(id) => id.clone(),
                                    None => return,
                                };
                                let (their_ephemeral_pk, nonce) = match validate_handshake(
                                    &their_handshake,
                                    &their_id,
                                    their_handshake.nonce.clone(),
                                ) {
                                    Ok(v) => v,
                                    Err(_) => return,
                                };
                                let (ephemeral_secret, our_handshake) =
                                    make_secret_and_handshake(&our, keypair.clone(), &their_id.name);
                                let _ =
                                    self_tx.send((NetworkMessage::HandshakeAck(our_handshake), None));
                                let encryption_key =
                                    ephemeral_secret.diffie_hellman(&their_ephemeral_pk);
                                let cipher = Aes256GcmSiv::new(&encryption_key.raw_secret_bytes());
                                let (sender_tx, sender_rx) =
                                    unbounded_channel::<(NetworkMessage, ErrorShuttle)>();
                                let (handler_tx, handler_rx) = unbounded_channel::<Vec<u8>>();

                                // spawn peer handler
                                let peer_handler = tokio::spawn(peer_handler(
                                    our.clone(),
                                    their_id.name.clone(),
                                    cipher,
                                    nonce,
                                    sender_rx,
                                    handler_rx,
                                    self_tx.clone(),
                                    kernel_message_tx.clone(),
                                ));

                                let peer = Peer {
                                    networking_address: their_id.networking_key,
                                    handle: peer_handler,
                                    sender: sender_tx.clone(),
                                    handler: handler_tx.clone(),
                                };
                                if let Some(old_peer) = peers.write().await.insert(their_id.name.clone(), peer) {
                                    let _ = old_peer.handle.abort();
                                }
                            } else {
                                // println!(
                                //     "got handshake from {} for {}\r",
                                //     their_handshake.from, their_handshake.target
                                // );
                                // a handshake should be forwarded to the target if possible.
                                // TODO discriminate and only do this for people we route for
                                if let Some(peer) = peers.write().await.get(&their_handshake.target) {
                                    let (result_tx, result_rx) = oneshot::channel::<MessageResult>();
                                    if let Ok(()) = peer.sender.send((
                                        NetworkMessage::Handshake(their_handshake),
                                        Some(result_tx),
                                    )) {
                                        if let Ok(Ok(Some(resp))) = result_rx.await {
                                            let _ = self_tx.send((resp, None));
                                            return;
                                        }
                                    }
                                }
                                // we cannot produce a connection to that target
                                // nack with id=0 will either be ignored or known as "handshake id"
                                let _ = self_tx.send((NetworkMessage::Nack(0), None));
                            }
                        });
                    }
                    NetworkMessage::Msg {
                        from,
                        to,
                        id,
                        contents,
                    } if to == our.name => {
                        // println!("got message for us\r");
                        let mut peers_write = peers.write().await;
                        if let Some(peer) = peers_write.get(&from) {
                            if let Ok(()) = peer.handler.send(contents) {
                                // println!("message handled and acked\r");
                                let _ = self_tx.send((NetworkMessage::Ack(id), None));
                                continue;
                            } else {
                                peers_write.remove(&from);
                            }
                        }
                        drop(peers_write);
                        // println!("message not handled\r");
                        // message was not handled, either kill connection if direct,
                        // or destroy peer if not.
                        if with == from {
                            // println!("...killing connection\r");
                            break;
                        }
                    }
                    NetworkMessage::Msg {
                        from,
                        to,
                        id,
                        contents,
                    } => {
                        // println!("got message for {to}\r");
                        // this message needs to be routed to someone else!
                        // TODO: be selective here!
                        // forward the ACK if we get it from target.
                        if let Some(peer) = peers.read().await.get(&to) {
                            let (result_tx, result_rx) = oneshot::channel::<MessageResult>();
                            if let Ok(()) = peer.sender.send((
                                NetworkMessage::Msg {
                                    from,
                                    to,
                                    id,
                                    contents,
                                },
                                Some(result_tx),
                            )) {
                                let self_tx = self_tx.clone();
                                tokio::spawn(async move {
                                    if let Ok(Ok(Some(NetworkMessage::Ack(id)))) = result_rx.await {
                                        return self_tx.send((NetworkMessage::Ack(id), None));
                                    } else {
                                        return self_tx.send((NetworkMessage::Nack(id), None));
                                    }
                                });
                                continue;
                            }
                            // we cannot send a message to that target
                            let _ = self_tx.send((NetworkMessage::Nack(id), None));
                        } else {
                            let _ = self_tx.send((NetworkMessage::Nack(id), None));
                        }
                    }
                }
            }
        }
    }
    // connection died, need to kill peer it was with
    let _ = peers.write().await.remove(&with);
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
    mut forwarder: UnboundedReceiver<(NetworkMessage, ErrorShuttle)>,
    mut receiver: UnboundedReceiver<Vec<u8>>,
    socket_tx: UnboundedSender<(NetworkMessage, ErrorShuttle)>,
    kernel_message_tx: MessageSender,
) {
    // println!("peer_handler\r");
    tokio::select! {
        //
        // take in messages from a specific peer, decrypt them, and send to kernel
        //
        _ = async {
            while let Some(encrypted_bytes) = receiver.recv().await {
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
            while let Some((message, result_tx)) = forwarder.recv().await {
                // if message is raw, we should encrypt.
                // otherwise, simply send
                match message {
                    NetworkMessage::Raw(message) => {
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
                    _ => {
                        if socket_tx.is_closed() {
                            result_tx
                                .unwrap()
                                .send(Err(SendErrorKind::Offline))
                                .unwrap();
                        } else {
                            let _ = socket_tx.send((message, result_tx));
                        }
                    }
                }
            }
        } => {},
    };
}
