use crate::types::*;
use crate::ws::*;
use aes_gcm_siv::{
    aead::{Aead, KeyInit},
    Aes256GcmSiv, Nonce,
};
use elliptic_curve::ecdh::EphemeralSecret;
use elliptic_curve::PublicKey;
use ethers::prelude::k256::Secp256k1;
use futures::stream::SplitStream;
use std::sync::Arc;
use tokio_tungstenite::connect_async;
use url::Url;

pub async fn handle_aggregate_connection(
    our: Identity,
    keypair: Arc<Ed25519KeyPair>,
    this_router: String,
    routers: Routers,
    read_stream: SplitStream<Sock>,
    pki: OnchainPKI,
    peers: Peers,
    self_message_tx: MessageSender,
    kernel_message_tx: MessageSender,
) -> Result<(), String> {
    let closed = aggregate_connection(
        our,
        keypair,
        this_router.clone(),
        routers,
        read_stream,
        pki,
        peers.clone(),
        self_message_tx.clone(),
        kernel_message_tx.clone(),
    )
    .await;
    println!("aggregate connection closed: {:#?}", closed);
    peers.write().await.remove(&this_router);
    return closed;
}

pub async fn handle_direct_connection(
    from: Identity,
    read_stream: SplitStream<Sock>,
    peers: Peers,
    ephemeral_secret: Arc<EphemeralSecret<Secp256k1>>,
    their_ephemeral_pk: Arc<PublicKey<Secp256k1>>,
    nonce: Arc<Nonce>,
    message_tx: MessageSender,
) -> Result<(), String> {
    let closed = direct_connection(
        from.clone(),
        read_stream,
        ephemeral_secret.clone(),
        their_ephemeral_pk,
        nonce,
        message_tx.clone(),
    )
    .await;
    println!("direct connection closed: {:#?}", closed);
    peers.write().await.remove(&from.name);
    return closed;
}

pub async fn handle_forwarding_connection(
    our: Identity,
    from: Identity,
    read_stream: SplitStream<Sock>,
    ephemeral_secret: Arc<EphemeralSecret<Secp256k1>>,
    their_ephemeral_pk: Arc<PublicKey<Secp256k1>>,
    nonce: Arc<Nonce>,
    message_tx: MessageSender,
    pki: OnchainPKI,
    peers: Peers,
    pass_throughs: PassThroughs,
) -> Result<(), String> {
    pass_throughs
        .write()
        .await
        .insert(from.name.clone(), HashMap::new());
    let closed = forwarding_connection(
        our,
        from.clone(),
        read_stream,
        ephemeral_secret,
        their_ephemeral_pk,
        nonce,
        message_tx,
        pki,
        peers.clone(),
        pass_throughs.clone(),
    )
    .await;
    println!("forwarding connection closed: {:#?}", closed);
    peers.write().await.remove(&from.name);
    // when we lose a forwarding connection, do teardown by going through
    // all pass-throughs with that peer and closing them.
    pass_throughs.write().await.remove(&from.name);
    return closed;
}

pub async fn _handle_pass_through_connection(
    from_1: Identity,
    write_stream_1: WriteStream,
    read_stream_1: SplitStream<Sock>,
    from_2: Identity,
    write_stream_2: WriteStream,
    read_stream_2: SplitStream<Sock>,
    peers: Peers,
) -> Result<(), String> {
    let closed = pass_through_connection(
        from_1.clone(),
        write_stream_1,
        read_stream_1,
        from_2,
        write_stream_2,
        read_stream_2,
    )
    .await;
    println!("pass-through connection closed: {:#?}", closed);
    peers.write().await.remove(&from_1.name);
    return closed;
}

/// used by a private node to receive messages from a routing node.
/// these messages can be sent by anyone
async fn aggregate_connection(
    our: Identity,
    keypair: Arc<Ed25519KeyPair>,
    this_router: String,
    routers: Routers,
    mut read_stream: SplitStream<Sock>,
    pki: OnchainPKI,
    peers: Peers,
    self_message_tx: MessageSender,
    kernel_message_tx: MessageSender,
) -> Result<(), String> {
    println!("aggregate connection!");
    while let Some(msg) = read_stream.next().await {
        let wrapped_message = match msg {
            Ok(msg) => match serde_json::from_slice::<WrappedMessage>(&msg.clone().into_data()) {
                Ok(v) => v,
                Err(_) => WrappedMessage::From(
                    serde_json::from_slice::<RoutedFrom>(&msg.into_data()).unwrap(),
                ),
            },
            Err(e) => return Err(format!("lost connection to router! error: {}", e)),
        };

        match wrapped_message {
            WrappedMessage::To(_) => return Err("this is a one-way connection".into()),
            WrappedMessage::LostPeer(lost) => {
                peers.write().await.remove(&lost);
            }
            WrappedMessage::PeerOffline(_lost) => {
                // XX propagate
                continue;
            }
            WrappedMessage::From(message) => {
                let peer_read = peers.read().await;
                let who: &Peer = match peer_read.get(&message.from) {
                    Some(v) => v,
                    None => continue,
                };

                let encryption_key = who.ephemeral_secret.diffie_hellman(&who.their_ephemeral_pk);
                let cipher = Aes256GcmSiv::new(&encryption_key.raw_secret_bytes());
                let decrypted = match cipher.decrypt(&who.nonce, message.contents.as_ref()) {
                    Ok(v) => v,
                    Err(e) => return Err(format!("error decrypting message: {}", e)),
                };
                let message = match serde_json::from_slice::<Message>(&decrypted) {
                    Ok(v) => v,
                    Err(e) => return Err(format!("error deserializing message: {}", e)),
                };

                // println!("\x1b[3;32m got message: {:?} \x1b[0m\n", message);
                match kernel_message_tx.send(vec![message]).await {
                    Ok(_) => {}
                    Err(e) => return Err(format!("error sending message: {}", e)),
                }
            }
            WrappedMessage::Handshake(handshake) => {
                // println!("getting that sweet handshake data");
                let their_id: Identity = match pki.get(&handshake.from) {
                    Some(v) => v.clone(),
                    None => continue,
                };
                // this is a bit weird
                match routers
                    .read()
                    .await
                    .get(&this_router)
                    .unwrap()
                    .pending_peers
                    .get(&their_id.name)
                {
                    Some((secret, nonce, message_to_send)) => {
                        // we initiated: get their handshake and start the connection
                        let (their_ephemeral_pk, nonce) =
                            validate_handshake(&handshake, &their_id, Some(nonce.to_vec()))?;
                        let peer = Peer {
                            networking_address: their_id.address,
                            ephemeral_secret: secret.clone(),
                            their_ephemeral_pk: their_ephemeral_pk.clone(),
                            nonce: nonce.clone(),
                            router: Some(this_router.clone()),
                            direct_write_stream: None,
                        };
                        peers.write().await.insert(their_id.name.clone(), peer);
                        let _err = self_message_tx.send(vec![message_to_send.clone()]).await;
                    }
                    None => {
                        // they initiated: validate, then make and send our handshake
                        let (their_ephemeral_pk, nonce) =
                            validate_handshake(&handshake, &their_id, handshake.nonce.clone())?;

                        let (ephemeral_secret, our_handshake) = make_secret_and_handshake(
                            &our,
                            keypair.clone(),
                            their_id.name.clone(),
                            Some(nonce.clone()),
                            false,
                        )?;

                        let peer = Peer {
                            networking_address: their_id.address,
                            ephemeral_secret: ephemeral_secret,
                            their_ephemeral_pk: their_ephemeral_pk,
                            nonce: nonce,
                            router: Some(this_router.clone()),
                            direct_write_stream: None,
                        };
                        let mut peer_write = peers.write().await;
                        peer_write.insert(their_id.name.clone(), peer);
                        let router = peer_write.get_mut(&this_router).unwrap();
                        match router.direct_write_stream.as_mut() {
                            Some(stream) => {
                                let _ = stream
                                    .send(tungstenite::Message::Binary(
                                        serde_json::to_vec(&WrappedMessage::Handshake(
                                            our_handshake,
                                        ))
                                        .unwrap(),
                                    ))
                                    .await;
                            }
                            None => {
                                return Err("lost connection to router".into());
                            }
                        }
                    }
                };
            }
        }
    }
    Err("connection loop closed".into())
}

/// used by a routing node to get messages from a non-public
/// node and send them to their target
async fn forwarding_connection(
    our: Identity,
    from: Identity,
    mut read_stream: SplitStream<Sock>,
    ephemeral_secret: Arc<EphemeralSecret<Secp256k1>>,
    their_ephemeral_pk: Arc<PublicKey<Secp256k1>>,
    nonce: Arc<Nonce>,
    message_tx: MessageSender,
    pki: OnchainPKI,
    peers: Peers,
    pass_throughs: PassThroughs,
) -> Result<(), String> {
    println!("forwarding connection!");
    // may need to build new connections here at times
    while let Some(msg) = read_stream.next().await {
        let wrapped_message = match msg {
            Ok(msg) => match serde_json::from_slice::<WrappedMessage>(&msg.into_data()) {
                Ok(v) => v,
                Err(e) => return Err(format!("error deserializing message: {}", e)),
            },
            Err(e) => return Err(format!("lost connection to routee! error: {}", e)),
        };

        match wrapped_message {
            WrappedMessage::To(msg) => {
                if &msg.to == &our.name {
                    let encryption_key = ephemeral_secret.diffie_hellman(&their_ephemeral_pk);
                    let cipher = Aes256GcmSiv::new(&encryption_key.raw_secret_bytes());
                    let decrypted = match cipher.decrypt(&nonce, msg.contents.as_ref()) {
                        Ok(v) => v,
                        Err(e) => return Err(format!("error decrypting message: {}", e)),
                    };
                    let message = match serde_json::from_slice::<Message>(&decrypted) {
                        Ok(v) => v,
                        Err(e) => return Err(format!("error deserializing message: {}", e)),
                    };
                    // println!("\x1b[3;32m got message: {:?} \x1b[0m\n", message);
                    match message_tx.send(vec![message]).await {
                        Ok(_) => {}
                        Err(e) => return Err(format!("error sending message: {}", e)),
                    }
                    continue;
                }

                let mut map_writer = pass_throughs.write().await;
                let map = map_writer.get_mut(&from.name).unwrap();

                let target_writer = match map.get_mut(&msg.to) {
                    Some(v) => v,
                    None => return Err("no direct connection to forward target yet".into()),
                };

                match target_writer
                    .send(tungstenite::Message::Binary(msg.contents))
                    .await
                {
                    Ok(_) => continue,
                    Err(_) => {
                        // we lost a forwarding target: share with the node at this
                        // connection so they can remove it from their peer-set
                        let _err = forward_special_message(
                            peers.clone(),
                            &from.name,
                            WrappedMessage::LostPeer(msg.to.clone()),
                        )
                        .await;
                        continue;
                    }
                }
            }
            WrappedMessage::Handshake(handshake) => {
                // if we have a pass-through for this already, use it
                // otherwise make a new one and send the handshake over it
                let mut map_writer = pass_throughs.write().await;
                let map = map_writer.get_mut(&from.name).unwrap();
                match map.get_mut(&handshake.target) {
                    Some(v) => {
                        let _ = v
                            .send(tungstenite::Message::Text(
                                serde_json::to_string(&handshake).unwrap(),
                            ))
                            .await;
                        continue;
                    }
                    None => {}
                }
                // no existing pass-through, make a new one
                let target_id: Identity = match pki.get(&handshake.target) {
                    Some(v) => v.clone(),
                    None => {
                        let _err = forward_special_message(
                            peers.clone(),
                            &from.name,
                            WrappedMessage::PeerOffline(handshake.target.clone()),
                        )
                        .await;
                        continue;
                    }
                };
                // if target has info, connect directly
                // otherwise, ask 1st router to connect and do a
                // self-send to retry message
                match &target_id.ws_routing {
                    Some((ip, port)) => {
                        // connect directly
                        let ws_url = Url::parse(&format!("ws://{}:{}/ws", ip, port)).unwrap();
                        match connect_async(ws_url).await {
                            Err(_) => {
                                let _err = forward_special_message(
                                    peers.clone(),
                                    &from.name,
                                    WrappedMessage::PeerOffline(handshake.target.clone()),
                                )
                                .await;
                                continue;
                            }
                            Ok((ws_stream, _response)) => {
                                let (mut write_stream, read_stream) = ws_stream.split();
                                // forward the handshake
                                let _send = match write_stream
                                    .send(tungstenite::Message::Text(
                                        serde_json::to_string(&handshake).unwrap(),
                                    ))
                                    .await
                                {
                                    Ok(_) => {}
                                    Err(_) => {
                                        let _err = forward_special_message(
                                            peers.clone(),
                                            &from.name,
                                            WrappedMessage::PeerOffline(handshake.target.clone()),
                                        )
                                        .await;
                                        continue;
                                    }
                                };

                                map.insert(handshake.target.clone(), write_stream);

                                tokio::spawn(one_way_pass_through_connection(
                                    from.name.clone(),
                                    target_id.name,
                                    read_stream,
                                    peers.clone(),
                                    None,
                                ));
                            }
                        }
                    }
                    None => {
                        // use a router
                        // TODO
                        return Err("XX".into());
                    }
                }
            }
            _ => {
                return Err("this is a one-way connection".into());
            }
        }
    }
    Err("connection loop closed".into())
}

async fn forward_special_message(
    peers: Peers,
    to: &str,
    msg: WrappedMessage,
) -> Result<(), String> {
    let mut peer_write = peers.write().await;
    match peer_write.get_mut(to) {
        None => Err("lol".into()),
        Some(peer) => match peer.direct_write_stream.as_mut() {
            Some(stream) => {
                match stream
                    .send(tungstenite::Message::Binary(
                        serde_json::to_vec(&msg).unwrap(),
                    ))
                    .await
                {
                    Ok(_) => Ok(()),
                    Err(_) => Err("lol".into()),
                }
            }
            None => Err("lol".into()),
        },
    }
}

/// used by a publicly accessible node to receive messages
/// from another publicly accessible node
async fn direct_connection(
    from: Identity,
    mut read_stream: SplitStream<Sock>,
    ephemeral_secret: Arc<EphemeralSecret<Secp256k1>>,
    their_ephemeral_pk: Arc<PublicKey<Secp256k1>>,
    nonce: Arc<Nonce>,
    message_tx: MessageSender,
) -> Result<(), String> {
    println!("direct connection!");
    while let Some(msg) = read_stream.next().await {
        match msg {
            Ok(msg) => {
                let encryption_key = ephemeral_secret.diffie_hellman(&their_ephemeral_pk);
                let cipher = Aes256GcmSiv::new(&encryption_key.raw_secret_bytes());
                let decrypted = match cipher.decrypt(&nonce, msg.into_data().as_ref()) {
                    Ok(v) => v,
                    Err(e) => return Err(format!("error decrypting message: {}", e)),
                };
                let message = match serde_json::from_slice::<Message>(&decrypted) {
                    Ok(v) => v,
                    Err(e) => return Err(format!("error deserializing message: {}", e)),
                };
                // println!("\x1b[3;32m got message: {:?} \x1b[0m\n", message);
                match message_tx.send(vec![message]).await {
                    Ok(_) => {}
                    Err(e) => return Err(format!("error sending message: {}", e)),
                }
            }
            Err(e) => {
                return Err(format!(
                    "lost connection to peer {}!\nerror: {}",
                    from.name, e
                ))
            }
        }
    }
    Err("connection loop closed".into())
}

pub async fn one_way_pass_through_connection(
    forward_to: String,
    from: String,
    mut read_stream: SplitStream<Sock>,
    peers: Peers,
    init: Option<Handshake>,
) {
    println!("one-way pass-through connection!");
    if init.is_some() {
        let hs = WrappedMessage::Handshake(init.unwrap());
        let wrapped_bytes = match serde_json::to_vec(&hs) {
            Ok(v) => v,
            Err(_) => return,
        };
        let mut peer_write = peers.write().await;
        match peer_write.get_mut(&forward_to) {
            Some(peer) => match peer.direct_write_stream.as_mut() {
                Some(stream) => {
                    match stream
                        .send(tungstenite::Message::Binary(wrapped_bytes))
                        .await
                    {
                        Ok(_) => {}
                        Err(_) => return,
                    }
                }
                None => return,
            },
            None => return,
        }
    }
    while let Some(msg) = read_stream.next().await {
        match msg {
            Ok(msg) => {
                // if message is a Text handshake, share that
                // convert message from raw to RoutedFrom
                let wrapped_message = match msg {
                    tungstenite::Message::Text(text) => {
                        WrappedMessage::Handshake(serde_json::from_str::<Handshake>(&text).unwrap())
                    }
                    _ => WrappedMessage::From(RoutedFrom {
                        from: from.clone(),
                        contents: msg.into_data(),
                    }),
                };

                let wrapped_bytes = match serde_json::to_vec(&wrapped_message) {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let mut peer_write = peers.write().await;
                match peer_write.get_mut(&forward_to) {
                    Some(peer) => match peer.direct_write_stream.as_mut() {
                        Some(stream) => {
                            match stream
                                .send(tungstenite::Message::Binary(wrapped_bytes))
                                .await
                            {
                                Ok(_) => continue,
                                Err(_) => break,
                            }
                        }
                        None => break,
                    },
                    None => break,
                }
            }
            Err(_) => break,
        }
    }
}

/// used by a routing node to receive messages from another
/// routing node and send them to
async fn pass_through_connection(
    from_1: Identity,
    write_stream_1: WriteStream,
    read_stream_1: SplitStream<Sock>,
    from_2: Identity,
    write_stream_2: WriteStream,
    read_stream_2: SplitStream<Sock>,
) -> Result<(), String> {
    tokio::select! {
        _ = pass_through(from_1, read_stream_1, write_stream_2) => { Err("connection closed".into()) },
        _ = pass_through(from_2, read_stream_2, write_stream_1) => { Err("connection closed".into()) },
    }
}

async fn pass_through(
    from: Identity,
    mut read: SplitStream<Sock>,
    mut write: SplitSink<Sock, tungstenite::Message>,
) {
    while let Some(msg) = read.next().await {
        match msg {
            Ok(msg) => {
                // convert message from RouteTo to RoutedFrom?
                match write.send(msg).await {
                    Ok(_) => continue,
                    Err(_) => break,
                }
            }
            Err(_) => break,
        }
    }
}
