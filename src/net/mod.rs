use crate::net::connections::*;
use crate::net::types::*;
use crate::types::*;
use aes_gcm_siv::Nonce;
use anyhow::Result;
use elliptic_curve::ecdh::EphemeralSecret;
use elliptic_curve::PublicKey;
use ethers::prelude::k256::{self, Secp256k1};
use ring::signature::{self, Ed25519KeyPair};
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
                keys.clone(),
                peers.clone(),
                kernel_message_tx.clone(),
            ))
        }
    };

    let _ = tokio::join!(listener, async {
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
            let (result_tx, result_rx) = oneshot::channel::<MessageResult>();
            let peers_read = peers.read().await;
            if let Some(peer) = peers_read.get(target) {
                //
                // we have the target as an active peer, meaning we can send the message directly
                //
                let _ = peer.sender.send((PeerMessage::Raw(km.clone()), result_tx));
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
            if let Some((peer_id, secret, nonce)) = keys.read().await.get(target) {
                //
                // we don't have the target as a peer yet, but we have shaken hands with them
                // before, and can try to reuse that shared secret to send a message.
                // first, we'll need to open a websocket and create a Peer struct for them.
                //
                if let Some((ref ip, ref port)) = peer_id.ws_routing {
                    //
                    // we can establish a connection directly with this peer
                    //
                    let Ok(ws_url) = make_ws_url(&our_ip, ip, port) else {
                        error_offline(km, &network_error_tx).await;
                        continue;
                    };
                    let Ok(Ok((websocket, _response))) =
                        timeout(TIMEOUT, connect_async(ws_url)).await
                    else {
                        error_offline(km, &network_error_tx).await;
                        continue;
                    };
                    let socket_tx = build_connection(
                        our.clone(),
                        keypair.clone(),
                        pki.clone(),
                        keys.clone(),
                        peers.clone(),
                        websocket,
                        kernel_message_tx.clone(),
                    )
                    .await;
                    let new_peer = create_new_peer(
                        &our,
                        &peer_id,
                        secret,
                        &nonce,
                        socket_tx.clone(),
                        kernel_message_tx.clone(),
                    );
                    peers.write().await.insert(peer_id.name.clone(), new_peer);
                    self_tx.send(km).await.unwrap();
                    continue;
                } else {
                    //
                    // need to find a router that will connect to this peer!
                    //
                    unimplemented!();
                }
            }
            //
            // sending a message to a peer for which we don't have active networking info.
            // this means that we need to search the PKI for the peer, and then attempt to
            // exchange handshakes with them.
            //
            let Some(peer_id) = pki.read().await.get(target).cloned() else {
                // this target cannot be found in the PKI!
                // throw an Offline error.
                error_offline(km, &network_error_tx).await;
                continue;
            };
            if let Some((ref ip, ref port)) = peer_id.ws_routing {
                //
                // we can establish a connection directly with this peer, then send a handshake
                //
                let Ok(ws_url) = make_ws_url(&our_ip, ip, port) else {
                    error_offline(km, &network_error_tx).await;
                    continue;
                };
                let Ok(Ok((websocket, _response))) = timeout(TIMEOUT, connect_async(ws_url)).await
                else {
                    error_offline(km, &network_error_tx).await;
                    continue;
                };
                let socket_tx = build_connection(
                    our.clone(),
                    keypair.clone(),
                    pki.clone(),
                    keys.clone(),
                    peers.clone(),
                    websocket,
                    kernel_message_tx.clone(),
                )
                .await;
                let (secret, handshake) =
                    make_secret_and_handshake(&our, keypair.clone(), target, None);
                // use the nonce from the initiatory handshake, always
                let nonce = *Nonce::from_slice(&handshake.nonce);
                let (handshake_tx, handshake_rx) = oneshot::channel::<MessageResult>();
                socket_tx
                    .send((NetworkMessage::Handshake(handshake), Some(handshake_tx)))
                    .unwrap();
                let response_shake = match timeout(TIMEOUT, handshake_rx).await {
                    Ok(Ok(Ok(Some(NetworkMessage::HandshakeAck(shake))))) => shake,
                    _ => {
                        println!("net: failed handshake with {target}\r");
                        error_offline(km, &network_error_tx).await;
                        continue;
                    }
                };
                let Ok(their_ephemeral_pk) = validate_handshake(&response_shake, &peer_id) else {
                    println!("net: failed handshake with {target}\r");
                    error_offline(km, &network_error_tx).await;
                    continue;
                };
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
                    socket_tx.clone(),
                    kernel_message_tx.clone(),
                );
                // can't do a self_tx.send here because we need to maintain ordering of messages
                // already queued.
                let _ = new_peer
                    .sender
                    .send((PeerMessage::Raw(km.clone()), result_tx));
                peers.write().await.insert(peer_id.name.clone(), new_peer);
                // now that the message is sent, spawn an async task to wait for the ack/nack/timeout
                tokio::spawn(wait_for_ack(
                    km.clone(),
                    peers.clone(),
                    target.to_string(),
                    result_rx,
                    network_error_tx.clone(),
                ));
                continue;
            } else {
                //
                // need to find a router that will connect to this peer, then do a handshake
                //
                unimplemented!();
            }
        }
    });
    Err(anyhow::anyhow!("networking task exited"))
}

async fn error_offline(km: KernelMessage, network_error_tx: &NetworkErrorSender) {
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
}

async fn wait_for_ack(
    km: KernelMessage,
    peers: Peers,
    target: String,
    result_rx: oneshot::Receiver<MessageResult>,
    network_error_tx: NetworkErrorSender,
) -> Result<Option<NetworkMessage>, SendErrorKind> {
    match timeout(TIMEOUT, result_rx).await {
        Ok(Ok(Ok(m))) => {
            return Ok(m);
        }
        Ok(Ok(Err(e))) => {
            let _ = peers.write().await.remove(&target);
            let _ = network_error_tx
                .send(WrappedSendError {
                    id: km.id,
                    source: km.source,
                    error: SendError {
                        kind: e.clone(),
                        target: km.target,
                        message: km.message,
                        payload: km.payload,
                    },
                })
                .await;
            return Err(e);
        }
        Ok(Err(e)) => {
            // RECV error: we couldn't even do a send to this peer..
            // TODO probably should trigger a retry here???
            println!("{e}\r");
            let _ = peers.write().await.remove(&target);
            let _ = error_offline(km, &network_error_tx).await;
            return Err(SendErrorKind::Offline);
        }
        Err(_e) => {
            // TIMEOUT error: we didn't get an ack in the fixed timeout period
            let _ = peers.write().await.remove(&target);
            let _ = error_offline(km, &network_error_tx).await;
            return Err(SendErrorKind::Offline);
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
        unimplemented!();

        // let _ = print_tx
        //     .send(Printout {
        //         verbosity: 0,
        //         content: format!("failed to connect to router: {router_name}"),
        //     })
        //     .await;
    }
}

/// only used if direct. should live forever
async fn receive_incoming_connections(
    our: Identity,
    keypair: Arc<Ed25519KeyPair>,
    port: u16,
    pki: OnchainPKI,
    keys: PeerKeys,
    peers: Peers,
    kernel_message_tx: MessageSender,
) {
    let tcp = TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .expect(format!("fatal error: can't listen on port {port}").as_str());

    while let Ok((stream, _socket_addr)) = tcp.accept().await {
        match accept_async(MaybeTlsStream::Plain(stream)).await {
            Ok(websocket) => {
                println!("received incoming connection\r");
                tokio::spawn(build_connection(
                    our.clone(),
                    keypair.clone(),
                    pki.clone(),
                    keys.clone(),
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

/// take in handshake and PKI identity, and confirm that the handshake is valid.
/// takes in optional nonce, which must be the one that connection initiator created.
fn validate_handshake(
    handshake: &Handshake,
    their_id: &Identity,
) -> Result<Arc<PublicKey<Secp256k1>>, String> {
    let their_networking_key = signature::UnparsedPublicKey::new(
        &signature::ED25519,
        hex::decode(&strip_0x(&their_id.networking_key))
            .map_err(|_| "failed to decode networking key")?,
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

    match PublicKey::<Secp256k1>::from_sec1_bytes(&handshake.ephemeral_public_key) {
        Ok(v) => return Ok(Arc::new(v)),
        Err(_) => return Err("error".into()),
    };
}

/// given an identity and networking key-pair, produces a handshake message along
/// with an ephemeral secret to be used in a specific connection.
fn make_secret_and_handshake(
    our: &Identity,
    keypair: Arc<Ed25519KeyPair>,
    target: &str,
    id: Option<u64>,
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

    // before signing our identity, convert router names to namehashes
    // to match the exact onchain representation of our identity
    let mut our_onchain_id = our.clone();
    our_onchain_id.allowed_routers = our
        .allowed_routers
        .clone()
        .into_iter()
        .map(|name| {
            let hash = crate::namehash(&name);
            let mut result = [0u8; 32];
            result.copy_from_slice(hash.as_bytes());
            format!("0x{}", hex::encode(result))
        })
        .collect();

    println!("signing id: {:?}\r", our_onchain_id);

    let signed_id = keypair
        .sign(&serde_json::to_vec(our).unwrap_or(vec![]))
        .as_ref()
        .to_vec();

    let mut iv = [0u8; 12];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut iv);
    let nonce = iv.to_vec();

    let handshake = Handshake {
        id: id.unwrap_or(rand::random()),
        from: our.name.clone(),
        target: target.to_string(),
        id_signature: signed_id,
        ephemeral_public_key: ephemeral_public_key.to_sec1_bytes().to_vec(),
        ephemeral_public_key_signature: signed_pk,
        nonce,
    };

    (ephemeral_secret, handshake)
}

fn strip_0x(s: &str) -> String {
    if s.starts_with("0x") {
        s[2..].to_string()
    } else {
        s.to_string()
    }
}
