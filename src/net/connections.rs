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
    if let Some(router_peer) = peers.write().await.get(&router) {
        //
        // if we have one of their routers as a peer already, try
        // and use that connection to first send a handshake,
        // then receive one, then create a peer-task and send the message
        //
        let target = initial_message.0.message.wire.target_ship.clone();
        // 1. generate a handshake
        let (ephemeral_secret, our_handshake) =
            make_secret_and_handshake(&our, keypair.clone(), &target);
        // 2. send the handshake
        let (result_tx, result_rx) = oneshot::channel::<MessageResult>();
        let _ = router_peer
            .sender
            .send((NetworkMessage::Handshake(our_handshake), Some(result_tx)));
        // 3. receive the target's handshake and validate it
        let their_handshake = match result_rx.await.unwrap() {
            Ok(Some(NetworkMessage::Handshake(h))) => h,
            _ => return Err(NetworkError::Offline),
        };
        let their_id = pki.read().await.get(&target).unwrap().clone();
        let (their_ephemeral_pk, nonce) =
            match validate_handshake(&their_handshake, &their_id, their_handshake.nonce.clone()) {
                Ok(v) => v,
                Err(_) => return Err(NetworkError::Offline),
            };
        let encryption_key = ephemeral_secret.diffie_hellman(&their_ephemeral_pk);
        let cipher = Aes256GcmSiv::new(&encryption_key.raw_secret_bytes());
        // 4. create Peer object and senders/handlers
        // sender -> socket, socket -> handler
        let (sender_tx, sender_rx) = unbounded_channel::<(NetworkMessage, ErrorShuttle)>();
        let (handler_tx, handler_rx) = unbounded_channel::<Vec<u8>>();

        let peer = Peer {
            networking_address: their_id.networking_key,
            is_ward: false,
            sender: sender_tx.clone(),
            handler: handler_tx.clone(),
            error: None,
        };
        // 5. spawn peer handler
        let peer_handler = tokio::spawn(peer_handler(
            our.clone(),
            their_id.name.clone(),
            cipher,
            nonce,
            sender_rx,
            handler_rx,
            router_peer.sender.clone(), // socket_tx.clone(),
            kernel_message_tx.clone(),
        ));
        // connection is now ready to write to
        tokio::spawn(active_routed_peer(
            their_id.name.clone(),
            peer_handler,
            peers.clone(),
        ));
        peers.write().await.insert(their_id.name.clone(), peer);
        println!("sending initial\r\n");
        let _ = sender_tx.send((NetworkMessage::Raw(initial_message.0), initial_message.1));
        return Ok(None);
    } else if let Some(router_id) = pki.read().await.get(&router) {
        if let Some((ip, port)) = &router_id.ws_routing {
            if let Ok(ws_url) = make_ws_url(&our_ip, ip, port) {
                if let Ok((websocket, _response)) = connect_async(ws_url).await {
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
    println!("build_connection\r\n");
    let (cipher, nonce, their_id) = match target {
        Some(target) => {
            // we have target, we are initiating
            let (ephemeral_secret, our_handshake) =
                make_secret_and_handshake(&our, keypair.clone(), &target.name);
            let _ = websocket
                .send(tungstenite::Message::Text(
                    serde_json::to_string(&our_handshake).unwrap(),
                ))
                .await;
            let their_handshake = get_handshake(&mut websocket).await.unwrap();
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
            let their_handshake = get_handshake(&mut websocket).await.unwrap();
            let their_id = pki.read().await.get(&their_handshake.from).unwrap().clone();
            let (their_ephemeral_pk, nonce) = match validate_handshake(
                &their_handshake,
                &their_id,
                their_handshake.nonce.clone(),
            ) {
                Ok(v) => v,
                Err(_) => return Err(NetworkError::Offline),
            };
            let (ephemeral_secret, our_handshake) =
                make_secret_and_handshake(&our, keypair.clone(), &their_id.name);
            let _ = websocket
                .send(tungstenite::Message::Text(
                    serde_json::to_string(&our_handshake).unwrap(),
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

    let peer = Peer {
        networking_address: their_id.networking_key,
        is_ward: false,
        sender: sender_tx.clone(),
        handler: handler_tx.clone(),
        error: None,
    };

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
    let connection_handler = tokio::spawn(maintain_connection(
        our.clone(),
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
        println!("sending initial\r\n");
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
    println!("active_peer\r\n");
    let _err = tokio::select! {
        _ = peer_handler => {},
        _ = connection_handler => {},
    };
    println!("lost active_peer, deleting peer!\r\n");
    peers.write().await.remove(&who);
    return who;
}

/// returns name of peer when it dies
async fn active_routed_peer(who: String, peer_handler: JoinHandle<()>, peers: Peers) -> String {
    println!("active_routed_peer\r\n");
    let _err = peer_handler.await;
    println!("lost active_routed_peer, deleting peer!\r\n");
    peers.write().await.remove(&who);
    return who;
}

/// send and receive messages on an existing websocket
/// if this breaks it's a timeout
async fn maintain_connection(
    our: Identity,
    keypair: Arc<Ed25519KeyPair>,
    pki: OnchainPKI,
    peers: Peers,
    self_tx: UnboundedSender<(NetworkMessage, ErrorShuttle)>,
    mut message_rx: UnboundedReceiver<(NetworkMessage, ErrorShuttle)>,
    websocket: WebSocket,
    kernel_message_tx: MessageSender,
) {
    println!("maintain_connection\r\n");
    let (mut write_stream, mut read_stream) = websocket.split();
    let (ack_tx, mut ack_rx) = unbounded_channel::<NetworkMessage>();

    let message_sender = tokio::spawn(async move {
        while let Some((message, result_tx)) = message_rx.recv().await {
            write_stream
                .send(tungstenite::Message::Binary(
                    serde_json::to_vec(&message).unwrap(),
                ))
                .await
                .expect("Failed to send a message");
            match message {
                NetworkMessage::Raw(_) => panic!("got raw message"), // can never happen
                NetworkMessage::Ack(_) => continue,
                NetworkMessage::Nack(_) => continue, // TODO what here?
                NetworkMessage::Error(_) => continue, // TODO what here?
                NetworkMessage::Msg { .. } | NetworkMessage::Handshake(_) => {
                    println!("message/handshake sent\r\n");
                    // await for the ack with timeout
                    match timeout(TIMEOUT, ack_rx.recv()).await {
                        Ok(Some(ack)) => {
                            // message delivered
                            println!("message/handshake delivered\r\n");
                            if let NetworkMessage::Handshake(_) = ack {
                                result_tx.unwrap().send(Ok(Some(ack))).unwrap();
                            } else {
                                result_tx.unwrap().send(Ok(None)).unwrap();
                            }
                            continue;
                        }
                        _ => {
                            result_tx.unwrap().send(Err(NetworkError::Timeout)).unwrap();
                            break;
                        }
                    }
                }
            }
        }
    });

    let message_receiver = tokio::spawn(async move {
        while let Some(msg) = read_stream.next().await {
            if let Ok(tungstenite::Message::Binary(bin)) = msg {
                if let Ok(msg) = serde_json::from_slice::<NetworkMessage>(&bin) {
                    match &msg {
                        NetworkMessage::Raw(_) => panic!("got raw message"), // can never happen
                        NetworkMessage::Ack(_) | NetworkMessage::Nack(_) => {
                            let _ = ack_tx.send(msg);
                            continue;
                        }
                        NetworkMessage::Error(_) => continue,
                        NetworkMessage::Handshake(their_handshake) => {
                            // if we get a handshake directed to us, respond with our own,
                            // and spawn an active_routed_peer!
                            if their_handshake.target == our.name {
                                let their_id =
                                    pki.read().await.get(&their_handshake.from).unwrap().clone();
                                let (their_ephemeral_pk, nonce) = match validate_handshake(
                                    &their_handshake,
                                    &their_id,
                                    their_handshake.nonce.clone(),
                                ) {
                                    Ok(v) => v,
                                    Err(_) => continue,
                                };
                                let (ephemeral_secret, our_handshake) =
                                    make_secret_and_handshake(&our, keypair, &their_id.name);
                                let _ =
                                    self_tx.send((NetworkMessage::Handshake(our_handshake), None));
                                let encryption_key =
                                    ephemeral_secret.diffie_hellman(&their_ephemeral_pk);
                                let cipher = Aes256GcmSiv::new(&encryption_key.raw_secret_bytes());
                                let (sender_tx, sender_rx) =
                                    unbounded_channel::<(NetworkMessage, ErrorShuttle)>();
                                let (handler_tx, handler_rx) = unbounded_channel::<Vec<u8>>();

                                let peer = Peer {
                                    networking_address: their_id.networking_key,
                                    is_ward: false,
                                    sender: sender_tx.clone(),
                                    handler: handler_tx.clone(),
                                    error: None,
                                };
                                // 5. spawn peer handler
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
                                if let Some(peer) = peers.write().await.get(&their_handshake.target)
                                {
                                    if let Ok(()) = peer.handler.send(bin.to_vec()) {
                                        let _ = self_tx.send((NetworkMessage::Ack(0), None));
                                    }
                                }
                                let _ = self_tx.send((NetworkMessage::Nack(0), None));
                            }
                        }
                        NetworkMessage::Msg { from, to, contents } => {
                            if to == &our.name {
                                if let Some(peer) = peers.write().await.get(from) {
                                    if let Ok(()) = peer.handler.send(contents.to_vec()) {
                                        // TODO id
                                        let _ = self_tx.send((NetworkMessage::Ack(0), None));
                                        continue;
                                    }
                                }
                            } else {
                                // this message needs to be routed to someone else!
                                // TODO: be selective here!
                                // TODO move this to a subprocess so it doesn't block our socket
                                // send a NACK if this doesn't work out.
                                // forward the ACK if we get it from target.
                                if let Some(peer) = peers.write().await.get(from) {
                                    let (result_tx, result_rx) =
                                        oneshot::channel::<MessageResult>();
                                    if let Ok(()) = peer.sender.send((msg, Some(result_tx))) {
                                        if let Ok(_) = result_rx.await.unwrap() {
                                            // TODO id
                                            let _ = self_tx.send((NetworkMessage::Ack(0), None));
                                            continue;
                                        }
                                    }
                                }
                                // NACK here
                                let _ = self_tx.send((NetworkMessage::Nack(0), None));
                                continue;
                            }
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
    println!("lost maintain_connection!\r\n");
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
    forwarder: UnboundedReceiver<(NetworkMessage, ErrorShuttle)>,
    mut receiver: UnboundedReceiver<Vec<u8>>,
    socket_tx: UnboundedSender<(NetworkMessage, ErrorShuttle)>,
    kernel_message_tx: MessageSender,
) {
    println!("peer_handler\r\n");
    let f_nonce = nonce.clone();
    let f_cipher = cipher.clone();

    let forwarder = tokio::spawn(async move {
        let socket_tx = socket_tx;
        let mut forwarder = forwarder;
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
                                    to: who.clone(),
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
        _ = async {
            while let Some(encrypted_bytes) = receiver.recv().await {
                // TODO manage failed decryptions?
                if let Ok(decrypted) = cipher.decrypt(&nonce, encrypted_bytes.as_ref()) {
                    if let Ok(message) = serde_json::from_slice::<WrappedMessage>(&decrypted) {
                        let _ = kernel_message_tx.send(message).await;
                    }
                }
            }
        } => {}
    };
    println!("lost peer_handler!\r\n");
}
