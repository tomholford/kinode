use crate::net::connections::{build_connection, create_new_peer};
use crate::net::types::*;
use crate::types::*;
use aes_gcm_siv::Nonce;
use anyhow::Result;
use elliptic_curve::ecdh::EphemeralSecret;
use elliptic_curve::PublicKey;
use ethers::prelude::k256::{self, Secp256k1};
use futures::StreamExt;
use ring::signature::{self, Ed25519KeyPair};
use std::collections::VecDeque;
use std::{collections::HashMap, sync::Arc};
use tokio::net::TcpListener;
use tokio::sync::{oneshot, RwLock};
use tokio::time::timeout;
use tokio_tungstenite::{accept_async, connect_async, MaybeTlsStream};

mod connections;
mod types;

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Entry point from the main kernel task. Runs forever, spawns listener and sender tasks.
pub async fn networking(
    our: Identity,
    our_ip: String,
    keypair: Arc<Ed25519KeyPair>,
    self_tx: MessageSender,
    kernel_message_tx: MessageSender,
    network_error_tx: NetworkErrorSender,
    print_tx: PrintSender,
    mut message_rx: MessageReceiver,
) -> Result<()> {
    // TODO persist this here
    let pki: OnchainPKI = Arc::new(RwLock::new(HashMap::new()));
    let keys: PeerKeys = Arc::new(RwLock::new(HashMap::new()));
    // mapping from QNS namehash to username
    let names: PKINames = Arc::new(RwLock::new(HashMap::new()));

    // this only lives during a given run of the kernel
    let peers: Peers = Arc::new(RwLock::new(HashMap::new()));

    // listener task either kickstarts our networking by establishing active connections
    // with one or more routers, or spawns a websocket listener if we are a direct node.
    let listener = match &our.ws_routing {
        None => {
            // indirect node: connect to router(s)
            tokio::spawn(connect_to_routers(
                our.clone(),
                keypair.clone(),
                our_ip.clone(),
                pki.clone(),
                peers.clone(),
                kernel_message_tx.clone(),
                print_tx.clone(),
            ))
        }
        Some((_ip, port)) => {
            // direct node: spawn the websocket listener
            tokio::spawn(receive_incoming_connections(
                our.clone(),
                keypair.clone(),
                *port,
                pki.clone(),
                peers.clone(),
                kernel_message_tx.clone(),
            ))
        }
    };

    tokio::join!(listener, async {
        while let Some(km) = message_rx.recv().await {
            // got a message from kernel to send out over the network
            let target = &km.target.node;
            if target == &our.name {
                // if the message is for us, it's either a protocol-level "hello" message,
                // or a debugging command issued from our terminal. handle it here:
                handle_incoming_message(
                    &our,
                    km,
                    peers.clone(),
                    pki.clone(),
                    names.clone(),
                    print_tx.clone(),
                )
                .await;
                continue;
            }
            let peers_read = peers.read().await;
            if let Some(peer) = peers_read.get(target) {
                //
                // we have the target as an active peer, meaning we can send the message directly
                //
                let (result_tx, result_rx) = oneshot::channel::<MessageResult>();
                let _ = peer
                    .sender
                    .send((PeerMessage::Raw(km.clone()), Some(result_tx)));
                // now that the message is sent, spawn an async task to wait for the ack/nack/timeout
                tokio::spawn(wait_for_ack(
                    km.clone(),
                    peers.clone(),
                    target.to_string(),
                    result_rx,
                    network_error_tx.clone(),
                ));
                continue;
            }
            drop(peers_read);
            let Some(peer_id) = pki.read().await.get(target).cloned() else {
                // this target cannot be found in the PKI!
                // throw an Offline error.
                let _ = network_error_tx
                    .send(WrappedSendError {
                        id: km.id,
                        source: km.source,
                        error: SendError {
                            kind: SendErrorKind::Offline,
                            target: km.target,
                            message: km.message,
                            payload: km.payload,
                        },
                    })
                    .await;
                continue;
            };
            if let Some((secret, nonce)) = keys.read().await.get(target) {
                //
                // we don't have the target as a peer yet, but we have shaken hands with them
                // before, and can try to reuse that shared secret to send a message.
                // first, we'll need to open a websocket and create a Peer struct for them.
                //
                if peer_id.ws_routing.is_some() {
                    //
                    // we can establish a connection directly with this peer
                    //
                    unimplemented!();
                } else {
                    //
                    // need to find a router that will connect to this peer!
                    //
                    unimplemented!();
                }
                continue;
            }
            // sending a message to a peer for which we don't have active networking info.
            // this means that we need to search the PKI for the peer, and then attempt to
            // exchange handshakes with them.
            unimplemented!();
            continue;
        }
    },);
    Err(anyhow::anyhow!("networking task exited"))
}

async fn wait_for_ack(
    km: KernelMessage,
    peers: Peers,
    target: String,
    result_rx: oneshot::Receiver<MessageResult>,
    network_error_tx: NetworkErrorSender,
) {
    match result_rx.await.unwrap_or(Err(SendErrorKind::Offline)) {
        Ok(_) => {
            // debugging stuff:
            // let end = std::time::Instant::now();
            // let elapsed = end.duration_since(start);
            // let _ = print_tx
            //     .send(Printout {
            //         verbosity: 0,
            //         content: format!(
            //             "sent ~{:.2}mb message to {target} in {elapsed:?}",
            //             bincode::serialize(&km).unwrap().len() as f64 / 1_048_576.0
            //         ),
            //     })
            //     .await;
            // end debugging stuff
            return;
        }
        Err(e) => {
            let _ = peers.write().await.remove(&target);
            let _ = network_error_tx
                .send(WrappedSendError {
                    id: km.id,
                    source: km.source,
                    error: SendError {
                        kind: e,
                        target: km.target,
                        message: km.message,
                        payload: km.payload,
                    },
                })
                .await;
            return;
        }
    }
}

/// Given a username, find a peer in the PKI and attempt to exchange handshakes with them.
/// If successful, send the original message to the peer and save them in our Peers mapping.
/// If a username doesn't exist to our knowledge, or we can't find a route to exchange
/// handshakes on, we return an Offline error.
async fn message_to_new_peer(
    our: Identity,
    our_ip: String,
    keypair: Arc<Ed25519KeyPair>,
    pki: OnchainPKI,
    peers: Peers,
    keys: PeerKeys,
    self_tx: MessageSender,
    kernel_message_tx: MessageSender,
    km: KernelMessage,
) -> Result<(), SendErrorKind> {
    // println!("sending message to unknown peer\r");
    let target = &km.target.node;
    // search PKI for peer and attempt to create a connection, then resend
    let pki_read = pki.read().await;
    match pki_read.get(target) {
        None => return Err(SendErrorKind::Offline),
        Some(peer_id) => {
            let peer_id = peer_id.clone();
            drop(pki_read);
            match &peer_id.ws_routing {
                //
                //  can connect directly to peer
                //
                Some((ip, port)) => {
                    if let Ok(ws_url) = make_ws_url(&our_ip, ip, port) {
                        if let Ok(Ok((websocket, _response))) =
                            timeout(TIMEOUT, connect_async(ws_url)).await
                        {
                            let first_message =
                                if let Some((secret, nonce)) = keys.read().await.get(target) {
                                    None
                                } else {
                                    let (ephemeral_secret, handshake) =
                                        make_secret_and_handshake(&our, keypair.clone(), target);
                                    Some(PeerMessage::Net(NetworkMessage::Handshake(handshake)))
                                };
                            if let Ok(conn_sender) = build_connection(
                                our.clone(),
                                keypair.clone(),
                                first_message,
                                pki.clone(),
                                peers.clone(),
                                websocket,
                                kernel_message_tx.clone(),
                            )
                            .await
                            {
                                if let Ok(peer) = create_new_peer(our, peer_id, conn_sender).await {
                                    peers.write().await.insert(peer.identity.name.clone(), peer);
                                    return Ok(());
                                }
                            }
                        }
                    }
                    return Err(SendErrorKind::Offline);
                }
                //
                //  peer does not have direct routing info, need to use router
                //
                None => {
                    let mut routers_to_try = VecDeque::from(peer_id.allowed_routers.clone());
                    while let Some(router) = routers_to_try.pop_front() {
                        if router == our.name {
                            continue;
                        }
                        if let Some(router_peer) = peers.read().await.get(&router) {
                            // use the router peer connection to send a handshake.
                            // use response handshake to generate a peer
                            // then send the original message to that peer.
                            // TODO
                            unimplemented!()
                        } else if let Some(router_id) = pki.read().await.get(&router) {
                            if let Some((ip, port)) = &router_id.ws_routing {
                                // println!("trying to connect to {router_name}\r");
                                if let Ok(ws_url) = make_ws_url(&our_ip, ip, port) {
                                    if let Ok(Ok((websocket, _response))) =
                                        timeout(TIMEOUT, connect_async(ws_url)).await
                                    {
                                        let (ephemeral_secret, handshake) =
                                            make_secret_and_handshake(
                                                &our,
                                                keypair.clone(),
                                                &router,
                                            );

                                        if let Ok(router_conn_sender) = build_connection(
                                            our.clone(),
                                            keypair.clone(),
                                            Some(PeerMessage::Net(NetworkMessage::Handshake(
                                                handshake,
                                            ))),
                                            pki.clone(),
                                            peers.clone(),
                                            websocket,
                                            kernel_message_tx.clone(),
                                        )
                                        .await
                                        {
                                            routers_to_try.push_front(router);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    //
                    // we tried all available routers and none of them worked!
                    //
                    return Err(SendErrorKind::Offline);
                }
            }
        }
    }
}

/// Only used if an indirect node.
async fn connect_to_routers(
    our: Identity,
    keypair: Arc<Ed25519KeyPair>,
    our_ip: String,
    pki: OnchainPKI,
    peers: Peers,
    kernel_message_tx: MessageSender,
    print_tx: PrintSender,
) {
    for router_name in &our.allowed_routers {
        if let Some(router_peer) = peers.read().await.get(router_name) {
            // we have this router as a peer already, all we need to do is open
            // a connection and give it to the peer entry.

            unimplemented!()
        } else if let Some(router_id) = pki.read().await.get(router_name).clone() {
            if let Some((ip, port)) = &router_id.ws_routing {
                // println!("trying to connect to {router_name}\r");
                if let Ok(ws_url) = make_ws_url(&our_ip, ip, port) {
                    if let Ok(Ok((websocket, _response))) =
                        timeout(TIMEOUT, connect_async(ws_url)).await
                    {
                        let (ephemeral_secret, handshake) =
                            make_secret_and_handshake(&our, keypair.clone(), router_name);

                        // this is a real and functional router! woohoo
                        if let Ok(conn_sender) = build_connection(
                            our.clone(),
                            keypair.clone(),
                            // there is an initial message: our handshake!
                            Some(PeerMessage::Net(NetworkMessage::Handshake(handshake))),
                            pki.clone(),
                            peers.clone(),
                            websocket,
                            kernel_message_tx.clone(),
                        )
                        .await
                        {
                            if let Ok(peer) =
                                create_new_peer(our.clone(), router_id.clone(), conn_sender).await
                            {
                                peers.write().await.insert(peer.identity.name.clone(), peer);
                                let _ = print_tx
                                    .send(Printout {
                                        verbosity: 0,
                                        content: format!("connected to router: {router_name}"),
                                    })
                                    .await;
                            }
                        }
                    }
                }
            }
        }
        let _ = print_tx
            .send(Printout {
                verbosity: 0,
                content: format!("failed to connect to router: {router_name}"),
            })
            .await;
    }
}

/// only used if direct. should live forever
async fn receive_incoming_connections(
    our: Identity,
    keypair: Arc<Ed25519KeyPair>,
    port: u16,
    pki: OnchainPKI,
    peers: Peers,
    kernel_message_tx: MessageSender,
) {
    let tcp = TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .expect(format!("fatal error: can't listen on port {port}").as_str());

    while let Ok((stream, _socket_addr)) = tcp.accept().await {
        match accept_async(MaybeTlsStream::Plain(stream)).await {
            Ok(websocket) => {
                tokio::spawn(build_connection(
                    our.clone(),
                    keypair.clone(),
                    None, // no known target / initial message
                    pki.clone(),
                    peers.clone(),
                    websocket,
                    kernel_message_tx.clone(),
                ));
            }
            // ignore connections we failed to accept
            Err(_) => {}
        }
    }
}

/// net module only handles requests, will never return a response
async fn handle_incoming_message(
    our: &Identity,
    km: KernelMessage,
    peers: Peers,
    pki: OnchainPKI,
    names: PKINames,
    print_tx: PrintSender,
) {
    let data = match km.message {
        Message::Response(_) => return,
        Message::Request(request) => match request.ipc {
            None => return,
            Some(ipc) => ipc,
        },
    };

    if km.source.node != our.name {
        let _ = print_tx
            .send(Printout {
                verbosity: 0,
                content: format!("\x1b[3;32m{}: {}\x1b[0m", km.source.node, data,),
            })
            .await;
    } else {
        // available commands: "peers", "QnsUpdate" (see qns_indexer module)
        // first parse as raw string, then deserialize to NetActions object
        match data.as_ref() {
            "peers" => {
                let peer_read = peers.read().await;
                let _ = print_tx
                    .send(Printout {
                        verbosity: 0,
                        content: format!("{:?}", peer_read.keys()),
                    })
                    .await;
            }
            "pki" => {
                let pki_read = pki.read().await;
                let _ = print_tx
                    .send(Printout {
                        verbosity: 0,
                        content: format!("{:?}", pki_read),
                    })
                    .await;
            }
            "names" => {
                let names_read = names.read().await;
                let _ = print_tx
                    .send(Printout {
                        verbosity: 0,
                        content: format!("{:?}", names_read),
                    })
                    .await;
            }
            _ => {
                let Ok(act) = serde_json::from_str::<NetActions>(&data) else {
                    let _ = print_tx
                        .send(Printout {
                            verbosity: 0,
                            content: "net: got unknown command".into(),
                        })
                        .await;
                    return;
                };
                match act {
                    NetActions::QnsUpdate(log) => {
                        if km.source.process != ProcessId::Name("qns_indexer".to_string()) {
                            let _ = print_tx
                                .send(Printout {
                                    verbosity: 0,
                                    content: "net: only qns_indexer can update qns data".into(),
                                })
                                .await;
                            return;
                        }
                        let _ = print_tx
                            .send(Printout {
                                verbosity: 0, // TODO 1
                                content: format!("net: got QNS update for {}", log.name),
                            })
                            .await;

                        let _ = pki.write().await.insert(
                            log.name.clone(),
                            Identity {
                                name: log.name.clone(),
                                networking_key: log.public_key,
                                ws_routing: if log.ip == "0.0.0.0".to_string() || log.port == 0 {
                                    None
                                } else {
                                    Some((log.ip, log.port))
                                },
                                allowed_routers: log.routers,
                            },
                        );
                        let _ = names.write().await.insert(log.node, log.name);
                    }
                }
            }
        }
    }
}

/*
 *  networking utils
 */

fn make_ws_url(our_ip: &str, ip: &str, port: &u16) -> Result<url::Url, SendErrorKind> {
    // if we have the same public IP as target, route locally,
    // otherwise they will appear offline due to loopback stuff
    let ip = if our_ip == ip { "localhost" } else { ip };
    match url::Url::parse(&format!("ws://{}:{}/ws", ip, port)) {
        Ok(v) => Ok(v),
        Err(_) => Err(SendErrorKind::Offline),
    }
}

/*
 *  handshake utils
 */

/// read one message from websocket stream and parse it as a handshake.
async fn get_handshake(websocket: &mut WebSocket) -> Result<Handshake, String> {
    let handshake_text = websocket
        .next()
        .await
        .ok_or("handshake failed")?
        .map_err(|e| format!("{}", e))?
        .into_text()
        .map_err(|e| format!("{}", e))?;
    let handshake: Handshake =
        serde_json::from_str(&handshake_text).map_err(|_| "got bad handshake")?;
    Ok(handshake)
}

/// take in handshake and PKI identity, and confirm that the handshake is valid.
/// takes in optional nonce, which must be the one that connection initiator created.
fn validate_handshake(
    handshake: &Handshake,
    their_id: &Identity,
    nonce: Vec<u8>,
) -> Result<(Arc<PublicKey<Secp256k1>>, Arc<Nonce>), String> {
    let their_networking_key = signature::UnparsedPublicKey::new(
        &signature::ED25519,
        hex::decode(&their_id.networking_key).map_err(|_| "failed to decode networking key")?,
    );

    if !(their_networking_key
        .verify(
            &serde_json::to_vec(&their_id).map_err(|_| "failed to serialize their identity")?,
            &handshake.id_signature,
        )
        .is_ok()
        && their_networking_key
            .verify(
                &handshake.ephemeral_public_key,
                &handshake.ephemeral_public_key_signature,
            )
            .is_ok())
    {
        // improper signatures on identity info, close connection
        return Err("got improperly signed networking info".into());
    }

    let their_ephemeral_pk =
        match PublicKey::<Secp256k1>::from_sec1_bytes(&handshake.ephemeral_public_key) {
            Ok(v) => Arc::new(v),
            Err(_) => return Err("error".into()),
        };

    // assign nonce based on our role in the connection
    let nonce = Arc::new(*Nonce::from_slice(&nonce));
    return Ok((their_ephemeral_pk, nonce));
}

/// given an identity and networking key-pair, produces a handshake message along
/// with an ephemeral secret to be used in a specific connection.
fn make_secret_and_handshake(
    our: &Identity,
    keypair: Arc<Ed25519KeyPair>,
    target: &str,
) -> (Arc<EphemeralSecret<Secp256k1>>, Handshake) {
    // produce ephemeral keys for DH exchange and subsequent symmetric encryption
    let ephemeral_secret = Arc::new(EphemeralSecret::<k256::Secp256k1>::random(
        &mut rand::rngs::OsRng,
    ));
    let ephemeral_public_key = ephemeral_secret.public_key();
    // sign the ephemeral public key with our networking management key
    let signed_pk = keypair
        .sign(&ephemeral_public_key.to_sec1_bytes())
        .as_ref()
        .to_vec();
    let signed_id = keypair
        .sign(&serde_json::to_vec(our).unwrap_or(vec![]))
        .as_ref()
        .to_vec();

    let mut iv = [0u8; 12];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut iv);
    let nonce = iv.to_vec();

    let handshake = Handshake {
        from: our.name.clone(),
        target: target.to_string(),
        id_signature: signed_id,
        ephemeral_public_key: ephemeral_public_key.to_sec1_bytes().to_vec(),
        ephemeral_public_key_signature: signed_pk,
        nonce,
    };

    (ephemeral_secret, handshake)
}
