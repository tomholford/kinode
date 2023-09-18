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

#[async_recursion]
pub async fn build_routed_connection(
    our: Identity,
    our_ip: String,
    keypair: Arc<Ed25519KeyPair>,
    router: String,
    initial_message: (KernelMessage, ErrorShuttle),
    pki: OnchainPKI,
    peers: Peers,
    kernel_message_tx: MessageSender,
) -> Result<(), NetworkErrorKind> {
    // println!("build_routed_connection\r");
    let peers_read = peers.read().await;
    if let Some(router_peer) = peers_read.get(&router) {
        //
        // if we have one of their routers as a peer already, try
        // and use that connection to first send a handshake,
        // then receive one, then create a peer-task and send the message
        //
        let target = initial_message.0.target.node.clone();
        let router_socket_tx = router_peer.sender.clone();
        // 1. generate a handshake
        let (ephemeral_secret, our_handshake) =
            make_secret_and_handshake(&our, keypair.clone(), &target);
        let nonce = our_handshake.nonce.clone();
        // 2. send the handshake
        let (result_tx, result_rx) = oneshot::channel::<MessageResult>();
        let _ = router_socket_tx.send((
            NetworkMessage::Handshake {
                id: 1,
                handshake: our_handshake,
            },
            Some(result_tx),
        ));
        drop(peers_read);
        // 3. receive the target's handshake and validate it
        let their_handshake = match result_rx.await.unwrap_or(Err(NetworkErrorKind::Timeout)) {
            Ok(Some(NetworkMessage::HandshakeAck { handshake, .. })) => handshake,
            Err(e) => return Err(e),
            _ => return Err(NetworkErrorKind::Offline),
        };
        let their_id = match pki.read().await.get(&target) {
            Some(id) => id.clone(),
            None => return Err(NetworkErrorKind::Offline),
        };
        let (their_ephemeral_pk, nonce) =
            match validate_handshake(&their_handshake, &their_id, nonce) {
                Ok(v) => v,
                Err(_) => return Err(NetworkErrorKind::Offline),
            };
        let encryption_key = ephemeral_secret.diffie_hellman(&their_ephemeral_pk);
        let cipher = Aes256GcmSiv::new(&encryption_key.raw_secret_bytes());
        // 4. create Peer object and senders/handlers
        // sender -> socket, socket -> handler
        let (sender_tx, sender_rx) = unbounded_channel::<(NetworkMessage, ErrorShuttle)>();
        let (handler_tx, handler_rx) = unbounded_channel::<Vec<u8>>();
        let (kill_tx, kill_rx) = unbounded_channel::<()>();
        let peer = Peer {
            networking_address: their_id.networking_key,
            sender: sender_tx.clone(),
            handler: handler_tx.clone(),
            destructor: kill_tx,
        };
        // 5. spawn peer handler
        let peer_handler = tokio::spawn(peer_handler(
            our.clone(),
            their_id.name.clone(),
            cipher,
            nonce,
            sender_rx,
            handler_rx,
            kill_rx,
            router_socket_tx,
            kernel_message_tx.clone(),
        ));
        // connection is now ready to write to
        tokio::spawn(active_routed_peer(
            their_id.name.clone(),
            sender_tx.clone(),
            peer_handler,
        ));
        peers.write().await.insert(their_id.name.clone(), peer);
        let _ = sender_tx.send((NetworkMessage::Raw(initial_message.0), initial_message.1));
        return Ok(());
    } else if let Some(router_id) = pki.read().await.get(&router) {
        drop(peers_read);
        if let Some((ip, port)) = &router_id.ws_routing {
            if let Ok(ws_url) = make_ws_url(&our_ip, ip, port) {
                if let Ok(Ok((websocket, _response))) =
                    timeout(TIMEOUT, connect_async(ws_url)).await
                {
                    //
                    // we were able to connect to one of their routers:
                    // try to connect to it normally as a peer, then
                    // recursively call this function to hit the above branch.
                    //
                    if let Ok(_) = build_connection(
                        our.clone(),
                        keypair.clone(),
                        Some(router_id.clone()),
                        None,
                        pki.clone(),
                        peers.clone(),
                        websocket,
                        kernel_message_tx.clone(),
                    )
                    .await
                    {
                        return build_routed_connection(
                            our,
                            our_ip,
                            keypair,
                            router,
                            initial_message,
                            pki.clone(),
                            peers.clone(),
                            kernel_message_tx,
                        )
                        .await;
                    }
                }
            }
        }
    }
    Err(NetworkErrorKind::Offline)
}

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
) -> Result<JoinHandle<String>, NetworkErrorKind> {
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
                Err(_) => return Err(NetworkErrorKind::Offline),
            };
            let (their_ephemeral_pk, nonce) =
                match validate_handshake(&their_handshake, &target, our_handshake.nonce.clone()) {
                    Ok(v) => v,
                    Err(_) => return Err(NetworkErrorKind::Offline),
                };
            let encryption_key = ephemeral_secret.diffie_hellman(&their_ephemeral_pk);
            let cipher = Aes256GcmSiv::new(&encryption_key.raw_secret_bytes());
            (cipher, nonce, target)
        }
        None => {
            // no target yet, wait for handshake to come in, then reply
            let their_handshake = match get_handshake(&mut websocket).await {
                Ok(h) => h,
                Err(_) => return Err(NetworkErrorKind::Offline),
            };
            let their_id = match pki.read().await.get(&their_handshake.from) {
                Some(id) => id.clone(),
                None => return Err(NetworkErrorKind::Offline),
            };
            let (their_ephemeral_pk, nonce) = match validate_handshake(
                &their_handshake,
                &their_id,
                their_handshake.nonce.clone(),
            ) {
                Ok(v) => v,
                Err(_) => return Err(NetworkErrorKind::Offline),
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
    let (kill_tx, kill_rx) = unbounded_channel::<()>();

    let peer = Peer {
        networking_address: their_id.networking_key,
        sender: sender_tx.clone(),
        handler: handler_tx.clone(),
        destructor: kill_tx,
    };

    let peer_handler = tokio::spawn(peer_handler(
        our.clone(),
        their_id.name.clone(),
        cipher,
        nonce,
        sender_rx,
        handler_rx,
        kill_rx,
        socket_tx.clone(),
        kernel_message_tx.clone(),
    ));
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
        peer_handler,
        connection_handler,
    ));
    // if this replaces an existing peer, destroy old one
    if let Some(old_peer) = peers.write().await.get(&their_id.name) {
        // println!("replacing existing dead-peer\r");
        let _ = old_peer.destructor.send(());
    }
    peers.write().await.insert(their_id.name.clone(), peer);
    if let Some(to_send) = initial_message {
        let _ = sender_tx.send(to_send);
    }
    Ok(active_peer)
}

/// returns name of peer when it dies
async fn active_peer(
    who: String,
    sender: UnboundedSender<(NetworkMessage, ErrorShuttle)>,
    peer_handler: JoinHandle<()>,
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
        _ = peer_handler => {},
        _ = connection_handler.join().unwrap() => {},
        _ = keepalive => {},
    }
    return who;
}

/// returns name of peer when it dies
async fn active_routed_peer(
    who: String,
    sender: UnboundedSender<(NetworkMessage, ErrorShuttle)>,
    peer_handler: JoinHandle<()>,
) -> String {
    let keepalive = tokio::spawn(async move {
        loop {
            let (result_tx, result_rx) = oneshot::channel::<MessageResult>();
            let _ = sender.send((NetworkMessage::Keepalive, Some(result_tx)));
            match result_rx.await {
                Ok(Ok(Some(NetworkMessage::Ack(_)))) => {
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                }
                _ => break,
            }
        }
    });
    // println!("active_routed_peer\r");
    tokio::select! {
        _ = peer_handler => {},
        _ = keepalive => {},
    }
    return who;
}

async fn ack_waiter(mut ack_rx: UnboundedReceiver<NetworkMessage>, shuttle: ErrorShuttle) {
    match timeout(TIMEOUT, ack_rx.recv()).await {
        Ok(Some(NetworkMessage::Nack(_))) => {
            let _ = shuttle.unwrap().send(Err(NetworkErrorKind::Offline));
        }
        Ok(Some(msg)) => {
            let _ = shuttle.unwrap().send(Ok(Some(msg)));
        }
        _ => {
            let _ = shuttle.unwrap().send(Err(NetworkErrorKind::Timeout));
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
                // can use a buffer here but doesn't seem to affect performance
                let bytes = bincode::serialize(&message).unwrap();
                println!("send of size: {:.2}mb\r", bytes.len() as f64 / 1_048_576.0);
                if bytes.len() > message_max_size {
                    // println!("message too large\r");
                    let _ = match result_tx {
                        Some(tx) => tx.send(Err(NetworkErrorKind::Timeout)),
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
                        println!("got ack\r");
                        if let Some(sender) = outstanding_acks.pop_back() {
                            let _ = sender.send(msg);
                        }
                    }
                    NetworkMessage::Keepalive => {
                        let _ = self_tx.send((NetworkMessage::Ack(0), None));
                    }
                    NetworkMessage::Handshake { id, handshake } => {
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
                                    self_tx.send((NetworkMessage::HandshakeAck { id, handshake: our_handshake }, None));
                                let encryption_key =
                                    ephemeral_secret.diffie_hellman(&their_ephemeral_pk);
                                let cipher = Aes256GcmSiv::new(&encryption_key.raw_secret_bytes());
                                let (sender_tx, sender_rx) =
                                    unbounded_channel::<(NetworkMessage, ErrorShuttle)>();
                                let (handler_tx, handler_rx) = unbounded_channel::<Vec<u8>>();
                                let (kill_tx, kill_rx) = unbounded_channel::<()>();

                                let peer = Peer {
                                    networking_address: their_id.networking_key,
                                    sender: sender_tx.clone(),
                                    handler: handler_tx.clone(),
                                    destructor: kill_tx,
                                };
                                // spawn peer handler
                                let peer_handler = tokio::spawn(peer_handler(
                                    our.clone(),
                                    their_id.name.clone(),
                                    cipher,
                                    nonce,
                                    sender_rx,
                                    handler_rx,
                                    kill_rx,
                                    self_tx.clone(),
                                    kernel_message_tx.clone(),
                                ));
                                // connection is now ready to write to
                                tokio::spawn(active_routed_peer(
                                    their_id.name.clone(),
                                    sender_tx.clone(),
                                    peer_handler,
                                ));
                                // if this replaces an existing peer, destroy old one
                                if let Some(old_peer) = peers.write().await.get(&their_id.name) {
                                    // println!("replacing existing dead-peer\r");
                                    let _ = old_peer.destructor.send(());
                                }
                                peers.write().await.insert(their_id.name.clone(), peer);
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
                                        NetworkMessage::Handshake { id, handshake: their_handshake },
                                        Some(result_tx),
                                    )) {
                                        if let Ok(Ok(Some(resp))) = result_rx.await {
                                            let _ = self_tx.send((resp, None));
                                            return;
                                        }
                                    } else {
                                        let _ = peer.destructor.send(());
                                    }
                                }
                                // we cannot produce a connection to that target
                                let _ = self_tx.send((NetworkMessage::Nack(id), None));
                            }
                        });
                    }
                    NetworkMessage::Msg {
                        from,
                        to,
                        id,
                        contents,
                    } if to == our.name => {
                        println!("got message for us\r");
                        if let Some(peer) = peers.write().await.get(&from) {
                            if let Ok(()) = peer.handler.send(contents) {
                                // println!("message handled and acked\r");
                                let _ = self_tx.send((NetworkMessage::Ack(id), None));
                                continue;
                            }
                        }
                        // println!("message not handled\r");
                        // message was not handled, either kill connection if direct,
                        // or destroy peer if not.
                        if with == from {
                            // println!("...killing connection\r");
                            break;
                        } else {
                            match peers.write().await.get(&from) {
                                None => {}
                                Some(peer) => {
                                    // println!("...removing peer\r");
                                    let _ = peer.destructor.send(());
                                }
                            }
                        }
                    }
                    NetworkMessage::Msg {
                        from,
                        to,
                        id,
                        contents,
                    } => {
                        println!("got message for {to}\r");
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
                            let _ = peer.destructor.send(());
                        } else {
                            let _ = self_tx.send((NetworkMessage::Nack(id), None));
                        }
                    }
                }
            }
        }
    }
    // connection died, need to kill peer it was with
    let _ = peers
        .write()
        .await
        .get_mut(&with)
        .unwrap()
        .destructor
        .send(());
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
    mut destructor: UnboundedReceiver<()>,
    socket_tx: UnboundedSender<(NetworkMessage, ErrorShuttle)>,
    kernel_message_tx: MessageSender,
) {
    // println!("peer_handler\r");

    let mut outstanding_acks = VecDeque::<UnboundedSender<NetworkMessage>>::new();

    tokio::select! {
        _ = async {
            while let Some((message, result_tx)) = forwarder.recv().await {
                // if message is raw, we should encrypt.
                // otherwise, simply send
                match message {
                    NetworkMessage::Raw(message) => {
                        if let Message::Request(ref r) = message.message {
                            println!("#{}\r", r.ipc.clone().unwrap_or_default());
                        }
                        if let Ok(bytes) = bincode::serialize::<KernelMessage>(&message) {
                            if let Ok(encrypted) = cipher.encrypt(&nonce, bytes.as_ref()) {
                                if socket_tx.is_closed() {
                                    let _ = result_tx.unwrap().send(Err(NetworkErrorKind::Offline));
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
                                .send(Err(NetworkErrorKind::Offline))
                                .unwrap();
                        } else {
                            let _ = socket_tx.send((message, result_tx));
                        }
                    }
                }
            }
        } => {
            // println!("peer_handler: forwarder died!\r");
        },
        _ = destructor.recv() => {
            // println!("peer_handler was killed!\r");
        },
        _ = async {
            while let Some(encrypted_bytes) = receiver.recv().await {
                if let Ok(decrypted) = cipher.decrypt(&nonce, encrypted_bytes.as_ref()) {
                    if let Ok(message) = bincode::deserialize::<KernelMessage>(&decrypted) {
                        // if let Message::Request(ref r) = message.message {
                        //     println!("#{}\r", r.ipc.clone().unwrap_or_default());
                        // }
                        let _ = kernel_message_tx.send(message).await;
                        continue;
                    }
                }
                // println!("net: decryption error with message from {who}\r");
                break;
            }
        } => {}
    };
}
