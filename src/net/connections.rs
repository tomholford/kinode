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

pub async fn build_routed_connection(
    our: Identity,
    our_ip: String,
    keypair: Arc<Ed25519KeyPair>,
    router: String,
    initial_message: (WrappedMessage, ErrorShuttle),
    pki: OnchainPKI,
    peers: Peers,
    kernel_message_tx: MessageSender,
) -> Result<Option<String>, NetworkError> {
    println!("build_routed_connection\r");
    let peers_write = peers.write().await;
    if let Some(router_peer) = peers_write.get(&router) {
        //
        // if we have one of their routers as a peer already, try
        // and use that connection to first send a handshake,
        // then receive one, then create a peer-task and send the message
        //
        let target = initial_message.0.message.wire.target_ship.clone();
        let router_socket_tx = router_peer.sender.clone();
        drop(peers_write);
        // 1. generate a handshake
        let (ephemeral_secret, our_handshake) =
            make_secret_and_handshake(&our, keypair.clone(), &target, true);
        let nonce = our_handshake.nonce.clone();
        // 2. send the handshake
        let (result_tx, result_rx) = oneshot::channel::<MessageResult>();
        let _ = router_socket_tx.send((NetworkMessage::Handshake(our_handshake), Some(result_tx)));
        // 3. receive the target's handshake and validate it
        let their_handshake = match result_rx.await.unwrap_or(Err(NetworkError::Timeout)) {
            Ok(Some(NetworkMessage::HandshakeAck(h))) => h,
            Err(e) => return Err(e),
            _ => return Err(NetworkError::Offline),
        };
        let their_id = match pki.read().await.get(&target) {
            Some(id) => id.clone(),
            None => return Err(NetworkError::Offline),
        };
        let (their_ephemeral_pk, nonce) =
            match validate_handshake(&their_handshake, &their_id, nonce) {
                Ok(v) => v,
                Err(_) => return Err(NetworkError::Offline),
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
            is_ward: false,
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
            peer_handler,
            peers.clone(),
        ));
        peers.write().await.insert(their_id.name.clone(), peer);
        let _ = sender_tx.send((NetworkMessage::Raw(initial_message.0), initial_message.1));
        return Ok(None);
    } else if let Some(router_id) = pki.read().await.get(&router) {
        drop(peers_write);
        if let Some((ip, port)) = &router_id.ws_routing {
            if let Ok(ws_url) = make_ws_url(&our_ip, ip, port) {
                if let Ok(Ok((websocket, _response))) =
                    timeout(TIMEOUT, connect_async(ws_url)).await
                {
                    //
                    // we were able to connect to one of their routers:
                    // try to connect to it normally as a peer, then
                    // return the name of the router to re-try it later
                    //
                    if let Ok(_) = build_connection(
                        our.clone(),
                        keypair,
                        Some(router_id.clone()),
                        None,
                        pki.clone(),
                        peers.clone(),
                        websocket,
                        kernel_message_tx.clone(),
                    )
                    .await
                    {
                        return Ok(Some(router));
                    }
                }
            }
        }
    }
    Err(NetworkError::Offline)
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
) -> Result<JoinHandle<String>, NetworkError> {
    println!("build_connection\r");
    let (cipher, nonce, their_id) = match target {
        Some(target) => {
            // we have target, we are initiating
            let (ephemeral_secret, our_handshake) =
                make_secret_and_handshake(&our, keypair.clone(), &target.name, true);
            let _ = websocket
                .send(tungstenite::Message::Text(
                    serde_json::to_string(&our_handshake).unwrap_or("".into()),
                ))
                .await;
            let their_handshake = match get_handshake(&mut websocket).await {
                Ok(h) => h,
                Err(_) => return Err(NetworkError::Offline),
            };
            let (their_ephemeral_pk, nonce) =
                match validate_handshake(&their_handshake, &target, our_handshake.nonce.clone()) {
                    Ok(v) => v,
                    Err(_) => return Err(NetworkError::Offline),
                };
            let encryption_key = ephemeral_secret.diffie_hellman(&their_ephemeral_pk);
            let cipher = Aes256GcmSiv::new(&encryption_key.raw_secret_bytes());
            (cipher, nonce, target)
        }
        None => {
            // no target yet, wait for handshake to come in, then reply
            let their_handshake = match get_handshake(&mut websocket).await {
                Ok(h) => h,
                Err(_) => return Err(NetworkError::Offline),
            };
            let their_id = match pki.read().await.get(&their_handshake.from) {
                Some(id) => id.clone(),
                None => return Err(NetworkError::Offline),
            };
            let (their_ephemeral_pk, nonce) = match validate_handshake(
                &their_handshake,
                &their_id,
                their_handshake.nonce.clone(),
            ) {
                Ok(v) => v,
                Err(_) => return Err(NetworkError::Offline),
            };
            let (ephemeral_secret, our_handshake) =
                make_secret_and_handshake(&our, keypair.clone(), &their_id.name, false);
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
        is_ward: false,
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
    let connection_handler = tokio::spawn(maintain_connection(
        our.clone(),
        their_id.name.clone(),
        keypair.clone(),
        pki.clone(),
        peers.clone(),
        socket_tx,
        socket_rx,
        websocket,
        kernel_message_tx.clone(),
    ));
    // connection is now ready to write to
    let active_peer = tokio::spawn(active_peer(
        their_id.name.clone(),
        peer_handler,
        connection_handler,
        peers.clone(),
    ));
    peers.write().await.insert(their_id.name.clone(), peer);
    if let Some(to_send) = initial_message {
        let _ = sender_tx.send(to_send);
    }
    Ok(active_peer)
}

/// returns name of peer when it dies
async fn active_peer(
    who: String,
    peer_handler: JoinHandle<()>,
    connection_handler: JoinHandle<()>,
    peers: Peers,
) -> String {
    println!("active_peer\r");
    let _err = tokio::select! {
        _ = peer_handler => {},
        _ = connection_handler => {},
    };
    peers.write().await.remove(&who);
    return who;
}

/// returns name of peer when it dies
async fn active_routed_peer(who: String, peer_handler: JoinHandle<()>, peers: Peers) -> String {
    println!("active_routed_peer\r");
    let _err = peer_handler.await;
    peers.write().await.remove(&who);
    return who;
}

/// send and receive messages on an existing websocket
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
    println!("maintain_connection\r");
    let (mut write_stream, mut read_stream) = websocket.split();
    let (ack_tx, mut ack_rx) = unbounded_channel::<NetworkMessage>();

    let s_our = our.clone();
    let s_peers = peers.clone();
    let message_sender = tokio::spawn(async move {
        while let Some((message, result_tx)) = message_rx.recv().await {
            write_stream
                .send(tungstenite::Message::Binary(
                    serde_json::to_vec(&message).unwrap(),
                ))
                .await
                .expect("Failed to send a message");
            match message {
                NetworkMessage::Raw(_) => break,
                NetworkMessage::Ack(_) => continue,
                NetworkMessage::Nack(_)
                | NetworkMessage::Error(_)
                | NetworkMessage::HandshakeAck(_) => continue,
                NetworkMessage::Handshake(_) => {
                    // if handshake was an initiator, await for response handshake
                    match timeout(TIMEOUT, ack_rx.recv()).await {
                        Ok(resp) => {
                            // response handshake received
                            result_tx.unwrap().send(Ok(resp)).unwrap();
                        }
                        _ => {
                            result_tx.unwrap().send(Err(NetworkError::Timeout)).unwrap();
                        }
                    }
                }
                NetworkMessage::Msg { from, to, .. } => {
                    // await for the ack with timeout
                    // TODO move this to a dedicated task for performance gainz?
                    match timeout(TIMEOUT, ack_rx.recv()).await {
                        Ok(Some(NetworkMessage::Ack(_))) => {
                            // message delivered
                            // TODO match acks, figure out how
                            // if id == ack_id {
                            result_tx.unwrap().send(Ok(None)).unwrap();
                            // } else {
                            //     println!("networking: ack mismatch!!\r");
                            //     result_tx.unwrap().send(Err(NetworkError::Offline)).unwrap();
                            // }
                        }
                        Err(_) => {
                            result_tx.unwrap().send(Err(NetworkError::Timeout)).unwrap();
                            if to == with {
                                break;
                            } else {
                                // send a kill message to our handler for that peer
                                println!("killing peer {to}\r");
                                let _ = s_peers.write().await.get(&to).unwrap().destructor.send(());
                            }
                        }
                        jej => {
                            println!("net: {:?}\r", jej);
                            result_tx.unwrap().send(Err(NetworkError::Offline)).unwrap();
                            if from == s_our.name && to == with {
                                break;
                            } else if from == s_our.name {
                                // send a kill message to our handler for that peer
                                println!("killing peer {to}\r");
                                let _ = s_peers.write().await.get(&to).unwrap().destructor.send(());
                            }
                        }
                    }
                }
            }
        }
    });

    // TODO clean up this mess
    let message_receiver = tokio::spawn(async move {
        while let Some(msg) = read_stream.next().await {
            if let Ok(tungstenite::Message::Binary(bin)) = msg {
                if let Ok(msg) = serde_json::from_slice::<NetworkMessage>(&bin) {
                    match msg {
                        NetworkMessage::Raw(_) => break,
                        NetworkMessage::Ack(_)
                        | NetworkMessage::Nack(_)
                        | NetworkMessage::HandshakeAck(_) => {
                            let _ = ack_tx.send(msg.clone());
                            continue;
                        }
                        NetworkMessage::Error(_) => break,
                        NetworkMessage::Handshake(their_handshake) => {
                            // if we get an initiatory handshake directed to us,
                            // respond with our own, and spawn an active_routed_peer!
                            let our = our.clone();
                            let pki = pki.clone();
                            let keypair = keypair.clone();
                            let self_tx = self_tx.clone();
                            let kernel_message_tx = kernel_message_tx.clone();
                            let peers = peers.clone();
                            let their_handshake = their_handshake.clone();
                            tokio::spawn(async move {
                                if their_handshake.target == our.name && their_handshake.init {
                                    let their_id = match pki.read().await.get(&their_handshake.from)
                                    {
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
                                        make_secret_and_handshake(
                                            &our,
                                            keypair.clone(),
                                            &their_id.name,
                                            false,
                                        );
                                    let _ = self_tx
                                        .send((NetworkMessage::HandshakeAck(our_handshake), None));
                                    let encryption_key =
                                        ephemeral_secret.diffie_hellman(&their_ephemeral_pk);
                                    let cipher =
                                        Aes256GcmSiv::new(&encryption_key.raw_secret_bytes());
                                    let (sender_tx, sender_rx) =
                                        unbounded_channel::<(NetworkMessage, ErrorShuttle)>();
                                    let (handler_tx, handler_rx) = unbounded_channel::<Vec<u8>>();
                                    let (kill_tx, kill_rx) = unbounded_channel::<()>();

                                    let peer = Peer {
                                        networking_address: their_id.networking_key,
                                        is_ward: false,
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
                                        peer_handler,
                                        peers.clone(),
                                    ));
                                    peers.write().await.insert(their_id.name.clone(), peer);
                                } else {
                                    // a handshake should be forwarded to the target if possible.
                                    // TODO discriminate and only do this for people we route for
                                    if let Some(peer) =
                                        peers.write().await.get(&their_handshake.target)
                                    {
                                        let (result_tx, result_rx) =
                                            oneshot::channel::<MessageResult>();
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
                                    // NACK here has id 0 because it's a response to a handshake
                                    println!("nack1\r");
                                    let _ = self_tx.send((NetworkMessage::Nack(0), None));
                                }
                            });
                            continue;
                        }
                        NetworkMessage::Msg {
                            from,
                            to,
                            id,
                            contents,
                        } if to == our.name => {
                            if let Some(peer) = peers.write().await.get(&from) {
                                if let Ok(()) = peer.handler.send(contents.to_vec()) {
                                    let _ = self_tx.send((NetworkMessage::Ack(id), None));
                                    continue;
                                }
                            }
                            println!("nack2\r");
                            let _ = self_tx.send((NetworkMessage::Nack(id), None));
                            continue;
                        }
                        NetworkMessage::Msg {
                            from,
                            to,
                            id,
                            contents,
                        } => {
                            // this message needs to be routed to someone else!
                            // TODO: be selective here!
                            // send a NACK if this doesn't work out.
                            // forward the ACK if we get it from target.
                            let self_tx = self_tx.clone();
                            let peers = peers.clone();
                            tokio::spawn(async move {
                                if let Some(peer) = peers.write().await.get(&to) {
                                    let (result_tx, result_rx) =
                                        oneshot::channel::<MessageResult>();
                                    if let Ok(()) = peer.sender.send((
                                        NetworkMessage::Msg {
                                            from,
                                            to,
                                            id,
                                            contents,
                                        },
                                        Some(result_tx),
                                    )) {
                                        if let Ok(Ok(None)) = result_rx.await {
                                            let _ = self_tx.send((NetworkMessage::Ack(id), None));
                                            return;
                                        }
                                    }
                                }
                                println!("nack3\r");
                                let _ = self_tx.send((NetworkMessage::Nack(id), None));
                            });
                            continue;
                        }
                    }
                }
            }
            // socket died
            break;
        }
    });

    // if either task completes, the socket is lost
    tokio::select! {
        _ = message_sender => {},
        _ = message_receiver => {},
    };
    println!("maintain_connection broke!\r");
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
    println!("peer_handler\r");

    let kill = tokio::spawn(async move {
        let _ = destructor.recv().await;
        println!("got kill command\r");
        return;
    });

    let f_nonce = nonce.clone();
    let f_cipher = cipher.clone();
    let f_who = who.clone();
    let forwarder = tokio::spawn(async move {
        while let Some((message, result_tx)) = forwarder.recv().await {
            // if message is raw, we should encrypt.
            // otherwise, simply send
            match message {
                NetworkMessage::Raw(message) => {
                    if let Ok(bytes) = serde_json::to_vec(&message) {
                        if let Ok(encrypted) = f_cipher.encrypt(&f_nonce, bytes.as_ref()) {
                            let _ = socket_tx.send((
                                NetworkMessage::Msg {
                                    from: our.name.clone(),
                                    to: f_who.clone(),
                                    id: message.id,
                                    contents: encrypted,
                                },
                                result_tx,
                            ));
                        }
                    }
                }
                _ => {
                    let _ = socket_tx.send((message, result_tx));
                    continue;
                }
            }
        }
    });

    tokio::select! {
        _ = forwarder => {},
        _ = kill => {},
        _ = async {
            while let Some(encrypted_bytes) = receiver.recv().await {
                if let Ok(decrypted) = cipher.decrypt(&nonce, encrypted_bytes.as_ref()) {
                    if let Ok(message) = serde_json::from_slice::<WrappedMessage>(&decrypted) {
                        let _ = kernel_message_tx.send(message).await;
                        continue;
                    }
                }
                println!("decryption error with message from {who}\r");
                break;
            }
        } => {}
    };
    println!("peer_handler broke!\r");
}
