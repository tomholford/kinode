// use crate::types::*;
use crate::types::{
    Message, MessageType, NetworkingError, Payload, Wire, WrappedMessage as KernelWrappedMessage,
};
use crate::ws::*;
use aes_gcm_siv::{
    aead::{Aead, KeyInit},
    Aes256GcmSiv,
};
use ring::signature::Ed25519KeyPair;
use std::sync::Arc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::{self};
use url::Url;

pub async fn ws_sender(
    our: Identity,
    keypair: Arc<Ed25519KeyPair>,
    pki: OnchainPKI,
    peers: Peers,
    kernel_message_tx: MessageSender,
    print_tx: PrintSender,
    mut message_rx: MessageReceiver,
    self_message_tx: MessageSender,
) {
    // initialization: if we are non-public, try sending a handshake
    // to each router, holding successful ones as aggregate connections
    let routers: Routers = Arc::new(RwLock::new(HashMap::new()));

    if our.ws_routing.is_none() {
        // TODO connect to all
        let router_name: &String = our
            .allowed_routers
            .get(0)
            .expect("fatal error: need at least one router if non-public");
        let result = connect_to_router(
            our.clone(),
            keypair.clone(),
            pki.clone(),
            peers.clone(),
            self_message_tx.clone(),
            kernel_message_tx.clone(),
            &router_name,
            routers.clone(),
        )
        .await;

        match result {
            Ok(_) => {
                routers.write().await.insert(
                    router_name.clone(),
                    Router {
                        name: router_name.clone(),
                        pending_peers: HashMap::new(),
                    },
                );
            }
            Err(e) => {
                panic!(
                    "fatal error: failed to connect to at least one router: {}",
                    e
                )
            }
        }
    }

    while let Some(message) = message_rx.recv().await {
        // interpret message: if directed to us, just print for now.
        // otherwise send to target.
        // can perform debug commands if we are the sender
        if message.message.wire.target_ship == our.name {
            if message.message.wire.source_ship != our.name {
                let _ = print_tx
                    .send(Printout {
                        verbosity: 0,
                        content: format!(
                            "\x1b[3;32m{}: {}\x1b[0m",
                            message.message.wire.source_ship,
                            message
                                .message
                                .payload
                                .json
                                .as_ref()
                                .unwrap_or(&serde_json::Value::Null),
                        ),
                    })
                    .await;
            } else {
                // available commands: peers
                match message
                    .message
                    .payload
                    .json
                    .as_ref()
                    .unwrap_or(&serde_json::Value::Null)
                {
                    serde_json::Value::String(s) => {
                        if s == "peers" {
                            let peer_read = peers.read().await;
                            let peers: Vec<(&String, &Peer)> = peer_read.iter().collect();
                            let _ = print_tx
                                .send(Printout {
                                    verbosity: 0,
                                    content: format!("{:#?}", peers),
                                })
                                .await;
                        }
                    }
                    _ => {
                        let _ = print_tx
                            .send(Printout {
                                verbosity: 1,
                                content: "ws: got unknown command".into(),
                            })
                            .await;
                    }
                }
            }
            continue;
        }

        let result = match our.ws_routing {
            Some(_) => {
                send_message(
                    &our,
                    keypair.clone(),
                    pki.clone(),
                    peers.clone(),
                    message.clone(),
                    kernel_message_tx.clone(),
                )
                .await
            }
            None => {
                // TODO "select" a router here
                send_message_routed(
                    &our,
                    routers.clone(),
                    keypair.clone(),
                    peers.clone(),
                    message.clone(),
                )
                .await
            }
        };

        match result {
            Ok(res) => match res {
                SuccessOrTimeout::Timeout => {
                    let _ = print_tx
                        .send(Printout {
                            verbosity: 1,
                            content: "ws: message timed out".into(),
                        })
                        .await;
                    if let MessageType::Request(false) = message.message.message_type {
                        continue;
                    }
                    let timeout_message = KernelWrappedMessage {
                        id: message.id.clone(),
                        rsvp: None,
                        message: Message {
                            message_type: MessageType::Response,
                            wire: Wire {
                                source_ship: our.name.clone(),
                                source_app: "ws".into(),
                                target_ship: our.name.clone(),
                                target_app: message.message.wire.source_app.clone(),
                            },
                            payload: Payload {
                                json: Some(
                                    serde_json::to_value(NetworkingError::MessageTimeout).unwrap(),
                                ),
                                bytes: None,
                            },
                        },
                    };
                    let _ = kernel_message_tx.send(timeout_message).await;
                }
                SuccessOrTimeout::TryAgain => {
                    let _ = self_message_tx.send(message.clone()).await;
                }
                SuccessOrTimeout::Success => {
                    if let MessageType::Request(false) = message.message.message_type {
                        continue;
                    }
                    let success_message = KernelWrappedMessage {
                        id: message.id.clone(),
                        rsvp: None,
                        message: Message {
                            message_type: MessageType::Response,
                            wire: Wire {
                                source_ship: our.name.clone(),
                                source_app: "ws".into(),
                                target_ship: our.name.clone(),
                                target_app: message.message.wire.source_app.clone(),
                            },
                            payload: Payload {
                                json: Some(
                                    serde_json::to_value(SuccessOrTimeout::Success).unwrap(),
                                ),
                                bytes: None,
                            },
                        },
                    };
                    let _ = kernel_message_tx.send(success_message).await;
                }
            },
            Err(e) => {
                let _ = print_tx
                    .send(Printout {
                        verbosity: 1,
                        content: format!("{}", e),
                    })
                    .await;
                if let MessageType::Request(false) = message.message.message_type {
                    continue;
                }

                let error_message = KernelWrappedMessage {
                    id: message.id.clone(),
                    rsvp: None,
                    message: Message {
                        message_type: MessageType::Response,
                        wire: Wire {
                            source_ship: our.name.clone(),
                            source_app: "ws".into(),
                            target_ship: our.name.clone(),
                            target_app: message.message.wire.source_app.clone(),
                        },
                        payload: Payload {
                            json: Some(serde_json::to_value(e).unwrap()),
                            bytes: None,
                        },
                    },
                };
                let _ = kernel_message_tx.send(error_message).await;
            }
        }
    }
}

async fn connect_to_router(
    our: Identity,
    keypair: Arc<Ed25519KeyPair>,
    pki: OnchainPKI,
    peers: Peers,
    self_message_tx: MessageSender,
    kernel_message_tx: MessageSender,
    router_name: &String,
    routers: Routers,
) -> Result<(), String> {
    let router: &Identity = pki
        .get(router_name)
        .ok_or("error: router not found in PKI")?;
    let router_ws_url: Url = match &router.ws_routing {
        Some((ip, port)) => match Url::parse(&format!("ws://{}:{}/ws", ip, port)) {
            Ok(v) => v,
            Err(_) => {
                return Err("error: failed to parse router websocket address".into());
            }
        },
        None => {
            return Err("error: router has no websocket address".into());
        }
    };
    match connect_async(router_ws_url).await {
        Err(_) => {
            return Err("error: failed to connect to router".into());
        }
        Ok((ws_stream, _response)) => {
            // create our handshake
            let (ephemeral_secret, our_handshake) =
                make_secret_and_handshake(&our, keypair.clone(), router_name.into(), None, true)?;

            let (mut write_stream, mut read_stream) = ws_stream.split();

            // send our handshake
            let _ = match write_stream
                .send(tungstenite::Message::Text(
                    serde_json::to_string(&our_handshake)
                        .map_err(|_| "failed to serialize handshake")?,
                ))
                .await
            {
                Ok(_) => (),
                Err(e) => return Err(format!("failed to send handshake: {}", e)),
            };

            // get the router's handshake
            let their_handshake: Handshake = get_handshake(&mut read_stream).await?;

            // verify handshake
            let (their_ephemeral_pk, nonce) =
                validate_handshake(&their_handshake, router, our_handshake.nonce.clone())?;

            peers.write().await.insert(
                router.name.clone(),
                Peer {
                    networking_address: router.address,
                    ephemeral_secret: ephemeral_secret.clone(),
                    their_ephemeral_pk: their_ephemeral_pk.clone(),
                    nonce: nonce.clone(),
                    router: None,
                    direct_write_stream: Some(write_stream),
                },
            );

            tokio::spawn(handle_aggregate_connection(
                our.clone(),
                keypair.clone(),
                router.name.clone(),
                routers.clone(),
                read_stream,
                pki.clone(),
                peers.clone(),
                self_message_tx.clone(),
                kernel_message_tx.clone(),
            ));
            Ok(())
        }
    }
}

async fn send_message(
    our: &Identity,
    keypair: Arc<Ed25519KeyPair>,
    pki: OnchainPKI,
    peers: Peers,
    message: KernelWrappedMessage,
    kernel_message_tx: MessageSender,
) -> Result<SuccessOrTimeout, NetworkingError> {
    // if we have an open write socket with target,
    // just write to that. if not, check for public
    // networking info. open direct connection if so,
    // otherwise make a pass-through with their router
    // and do a self-send to retry message
    let target = &message.message.wire.target_ship;
    let message_bytes = match serde_json::to_vec(&message) {
        Ok(v) => v,
        Err(_) => return Err(NetworkingError::NetworkingBug),
    };

    let mut peer_write = peers.write().await;
    match peer_write.get_mut(target) {
        Some(peer) => {
            let encryption_key = peer
                .ephemeral_secret
                .diffie_hellman(&peer.their_ephemeral_pk);
            let cipher = Aes256GcmSiv::new(&encryption_key.raw_secret_bytes());
            let encrypted = match cipher.encrypt(&peer.nonce, message_bytes.as_ref()) {
                Ok(v) => v,
                Err(_) => return Err(NetworkingError::NetworkingBug),
            };

            match peer.direct_write_stream.as_mut() {
                None => Err(NetworkingError::PeerOffline),
                Some(stream) => {
                    // if we're their router, wrap in a RoutedFrom
                    let wrapped = match &peer.router {
                        None => encrypted,
                        Some(their_router) => {
                            if their_router == &our.name {
                                match serde_json::to_vec(&WrappedMessage::From {
                                    from: our.name.clone(),
                                    contents: encrypted,
                                }) {
                                    Ok(v) => v,
                                    Err(_) => return Err(NetworkingError::NetworkingBug),
                                }
                            } else {
                                encrypted
                            }
                        }
                    };
                    match stream.send(tungstenite::Message::Binary(wrapped)).await {
                        Ok(_) => return Ok(SuccessOrTimeout::Success),
                        Err(_) => return Err(NetworkingError::MessageTimeout),
                    }
                }
            }
        }
        None => {
            drop(peer_write);
            // no connection with target, check for public networking info
            let target_id: &Identity = match pki.get(target) {
                Some(v) => v,
                None => return Err(NetworkingError::PeerOffline),
            };
            // if target has info, connect directly
            // otherwise, ask 1st router to connect and do a
            // self-send to retry message
            match &target_id.ws_routing {
                Some((ip, port)) => {
                    // connect directly
                    let ws_url = match Url::parse(&format!("ws://{}:{}/ws", ip, port)) {
                        Ok(v) => v,
                        Err(_) => return Err(NetworkingError::PeerOffline),
                    };
                    match connect_async(ws_url).await {
                        Err(_) => {
                            return Err(NetworkingError::PeerOffline);
                        }
                        Ok((ws_stream, _response)) => {
                            let (mut write_stream, mut read_stream) = ws_stream.split();

                            // create our handshake
                            let (ephemeral_secret, our_handshake) = match make_secret_and_handshake(
                                our,
                                keypair.clone(),
                                target.into(),
                                None,
                                false,
                            ) {
                                Ok(v) => v,
                                Err(_) => return Err(NetworkingError::NetworkingBug),
                            };

                            // send our handshake
                            let _ = match write_stream
                                .send(tungstenite::Message::Text(
                                    serde_json::to_string(&our_handshake).unwrap(),
                                ))
                                .await
                            {
                                Ok(_) => (),
                                Err(_) => return Err(NetworkingError::PeerOffline),
                            };

                            // get the target's handshake
                            let their_handshake: Handshake =
                                match get_handshake(&mut read_stream).await {
                                    Ok(v) => v,
                                    Err(_) => return Err(NetworkingError::PeerOffline),
                                };

                            // verify handshake
                            let (their_ephemeral_pk, nonce) = match validate_handshake(
                                &their_handshake,
                                &target_id,
                                our_handshake.nonce.clone(),
                            ) {
                                Ok(v) => v,
                                Err(_) => return Err(NetworkingError::PeerOffline),
                            };

                            peers.write().await.insert(
                                target.clone(),
                                Peer {
                                    networking_address: target_id.address,
                                    ephemeral_secret: ephemeral_secret.clone(),
                                    their_ephemeral_pk: their_ephemeral_pk.clone(),
                                    nonce: nonce.clone(),
                                    router: None,
                                    direct_write_stream: Some(write_stream),
                                },
                            );

                            tokio::spawn(handle_direct_connection(
                                target_id.clone(),
                                read_stream,
                                peers.clone(),
                                ephemeral_secret.clone(),
                                their_ephemeral_pk,
                                nonce,
                                kernel_message_tx.clone(),
                            ));

                            return Ok(SuccessOrTimeout::TryAgain);
                        }
                    }
                }
                None => {
                    // use a router
                    // for now, just try the first one
                    let router_name = target_id.allowed_routers.get(0).unwrap();

                    // find the router
                    let router: &Identity = match pki.get(router_name) {
                        Some(v) => v,
                        None => return Err(NetworkingError::PeerOffline),
                    };

                    let (ip, port) = match &router.ws_routing {
                        Some(v) => v,
                        None => return Err(NetworkingError::PeerOffline),
                    };

                    // connect to router
                    let ws_url = match Url::parse(&format!("ws://{}:{}/ws", ip, port)) {
                        Ok(v) => v,
                        Err(_) => return Err(NetworkingError::PeerOffline),
                    };
                    match connect_async(ws_url).await {
                        Err(_) => {
                            return Err(NetworkingError::PeerOffline);
                        }
                        Ok((ws_stream, _response)) => {
                            let (mut write_stream, mut read_stream) = ws_stream.split();

                            // create our handshake
                            let (ephemeral_secret, our_handshake) = match make_secret_and_handshake(
                                our,
                                keypair.clone(),
                                target.into(),
                                None,
                                false,
                            ) {
                                Ok(v) => v,
                                Err(_) => return Err(NetworkingError::NetworkingBug),
                            };
                            // send our handshake
                            let _ = match write_stream
                                .send(tungstenite::Message::Text(
                                    serde_json::to_string(&our_handshake).unwrap(),
                                ))
                                .await
                            {
                                Ok(_) => (),
                                Err(_) => return Err(NetworkingError::PeerOffline),
                            };
                            // get the target's handshake
                            // XX NEED A MANUAL TIMEOUT HERE?
                            let their_handshake: Handshake =
                                match get_handshake(&mut read_stream).await {
                                    Ok(v) => v,
                                    Err(_) => return Err(NetworkingError::PeerOffline),
                                };
                            // verify handshake
                            let (their_ephemeral_pk, nonce) = match validate_handshake(
                                &their_handshake,
                                &target_id,
                                our_handshake.nonce.clone(),
                            ) {
                                Ok(v) => v,
                                Err(_) => return Err(NetworkingError::PeerOffline),
                            };
                            peers.write().await.insert(
                                target.clone(),
                                Peer {
                                    networking_address: target_id.address,
                                    ephemeral_secret: ephemeral_secret.clone(),
                                    their_ephemeral_pk: their_ephemeral_pk.clone(),
                                    nonce: nonce.clone(),
                                    router: Some(router_name.into()),
                                    direct_write_stream: Some(write_stream),
                                },
                            );
                            tokio::spawn(handle_direct_connection(
                                target_id.clone(),
                                read_stream,
                                peers.clone(),
                                ephemeral_secret.clone(),
                                their_ephemeral_pk,
                                nonce,
                                kernel_message_tx.clone(),
                            ));

                            Ok(SuccessOrTimeout::TryAgain)
                        }
                    }
                }
            }
        }
    }
}

async fn send_message_routed(
    our: &Identity,
    routers: Routers,
    keypair: Arc<Ed25519KeyPair>,
    peers: Peers,
    message: KernelWrappedMessage,
) -> Result<SuccessOrTimeout, NetworkingError> {
    // if the target is a router, send on our conn with them.
    // if target has a router marked, send on that conn.
    // otherwise, ask 1st router to connect and do a
    // self-send to retry message
    let target = &message.message.wire.target_ship;

    let mut peer_write = peers.write().await;
    match peer_write.get_mut(target) {
        Some(peer) => {
            let message_bytes = match serde_json::to_vec(&message) {
                Ok(v) => v,
                Err(_) => return Err(NetworkingError::NetworkingBug),
            };
            let encryption_key = peer
                .ephemeral_secret
                .diffie_hellman(&peer.their_ephemeral_pk);
            let cipher = Aes256GcmSiv::new(&encryption_key.raw_secret_bytes());
            let encrypted = match cipher.encrypt(&peer.nonce, message_bytes.as_ref()) {
                Ok(v) => v,
                Err(_) => return Err(NetworkingError::NetworkingBug),
            };

            let wrapped: WrappedMessage = WrappedMessage::To {
                to: target.clone(),
                contents: encrypted,
            };

            let wrapped_bytes = serde_json::to_vec(&wrapped).unwrap();

            match peer.direct_write_stream.as_mut() {
                Some(stream) => match stream
                    .send(tungstenite::Message::Binary(wrapped_bytes))
                    .await
                {
                    Ok(_) => return Ok(SuccessOrTimeout::Success),
                    Err(_) => return Err(NetworkingError::MessageTimeout),
                },
                None => {
                    // use our router to send the message
                    let router = match peer.router.clone() {
                        Some(v) => v,
                        None => return Err(NetworkingError::PeerOffline),
                    };
                    match peer_write.get_mut(&router) {
                        Some(router) => match router.direct_write_stream.as_mut() {
                            Some(stream) => {
                                match stream
                                    .send(tungstenite::Message::Binary(wrapped_bytes))
                                    .await
                                {
                                    Ok(_) => return Ok(SuccessOrTimeout::Success),
                                    Err(_) => return Err(NetworkingError::MessageTimeout),
                                }
                            }
                            None => {
                                return Err(NetworkingError::PeerOffline);
                            }
                        },
                        None => {
                            return Err(NetworkingError::PeerOffline);
                        }
                    }
                }
            }
        }
        None => {
            // we don't have this node as a peer, need to ask one of our routers to connect us
            // TODO expand on this, select a router for real
            let mut routers_write = routers.write().await;
            let router = routers_write.iter_mut().next().unwrap().1;

            // create a handshake for them to forward
            let (ephemeral_secret, our_handshake) =
                match make_secret_and_handshake(our, keypair.clone(), target.into(), None, false) {
                    Ok(v) => v,
                    Err(_) => return Err(NetworkingError::NetworkingBug),
                };

            // save ephemeral secret to use later, and message to send at that point
            router.pending_peers.insert(
                target.clone(),
                (
                    ephemeral_secret.clone(),
                    our_handshake.nonce.clone().unwrap(),
                    message,
                ),
            );

            // send handshake to our router
            // we will get response as a message from them back on our aggregate connection
            let wrapped: WrappedMessage = WrappedMessage::Handshake(our_handshake);
            let wrapped_bytes = serde_json::to_vec(&wrapped).unwrap();

            let router = match peer_write.get_mut(&router.name) {
                Some(v) => v,
                None => return Err(NetworkingError::PeerOffline),
            };

            match router.direct_write_stream.as_mut() {
                Some(stream) => match stream
                    .send(tungstenite::Message::Binary(wrapped_bytes))
                    .await
                {
                    Ok(_) => return Ok(SuccessOrTimeout::Success),
                    Err(_) => return Err(NetworkingError::MessageTimeout),
                },
                None => {
                    return Err(NetworkingError::PeerOffline);
                }
            }
        }
    }
}
