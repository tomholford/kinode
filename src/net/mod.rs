use std::collections::VecDeque;
use std::{collections::HashMap, sync::Arc};

use crate::net::connections::build_connection;
use crate::types::*;

use aes_gcm_siv::Nonce;
use anyhow::Result;
use async_recursion::async_recursion;
use elliptic_curve::ecdh::EphemeralSecret;
use elliptic_curve::PublicKey;
use ethers::prelude::k256::{self, Secp256k1};
use futures::StreamExt;
use ring::signature::{self, Ed25519KeyPair};
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio::task::JoinSet;
use tokio::time::timeout;
use tokio_tungstenite::{accept_async, connect_async, MaybeTlsStream, WebSocketStream};

mod connections;

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

type Peers = Arc<RwLock<HashMap<String, Peer>>>;
type WebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type MessageResult = Result<Option<NetworkMessage>, NetworkErrorKind>;
type ErrorShuttle = Option<oneshot::Sender<MessageResult>>;

// stored in mapping by their username
pub struct Peer {
    pub networking_address: String,
    handle: tokio::task::JoinHandle<()>,
    pub sender: mpsc::UnboundedSender<(NetworkMessage, ErrorShuttle)>,
    pub handler: mpsc::UnboundedSender<Vec<u8>>,
}

/// parsed from Binary websocket message on an Indirect route
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NetworkMessage {
    Ack(u64),
    Nack(u64),
    Msg {
        from: String,
        to: String,
        id: u64,
        contents: Vec<u8>,
    },
    Raw(KernelMessage),
    Handshake(Handshake),
    HandshakeAck(Handshake),
    Keepalive,
}

/// contains identity and encryption keys, used in initial handshake.
/// parsed from Text websocket message
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Handshake {
    from: String,
    target: String,
    id_signature: Vec<u8>,
    ephemeral_public_key: Vec<u8>,
    ephemeral_public_key_signature: Vec<u8>,
    nonce: Vec<u8>,
}

pub async fn networking(
    our: Identity,
    our_ip: String,
    keypair: Arc<Ed25519KeyPair>,
    pki: OnchainPKI,
    self_tx: MessageSender,
    kernel_message_tx: MessageSender,
    network_error_tx: NetworkErrorSender,
    print_tx: PrintSender,
    mut message_rx: MessageReceiver,
) -> Result<()> {
    // TODO: persist this if we shutdown gracefully
    let peers: Peers = Arc::new(RwLock::new(HashMap::new()));

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

    tokio::select! {
        _listener = listener => Err(anyhow::anyhow!("listener died")),
        _sender = async {
            while let Some(km) = message_rx.recv().await {
                // got a message from kernel to send out over the network
                // debugging stuff:
                // if let Message::Request(ref r) = km.message {
                //     println!("A #{}\r", r.ipc.as_ref().unwrap_or(&"".to_string()));
                // }
                // let start = std::time::Instant::now();
                // end debugging stuff
                let target = &km.target.node;
                if target == &our.name {
                    handle_incoming_message(&our, km, peers.clone(), print_tx.clone()).await;
                    continue;
                }
                let peers_read = peers.read().await;
                if let Some(peer) = peers_read.get(target) {
                    // if we have the peer, simply send the message to their sender
                    let (result_tx, result_rx) = oneshot::channel::<MessageResult>();
                    let _ = peer
                        .sender
                        .send((NetworkMessage::Raw(km.clone()), Some(result_tx)));
                    drop(peers_read);
                    // now that the message is sent, spawn an async task to wait for the ack/nack/timeout
                    let peers = peers.clone();
                    let target = target.clone();
                    let network_error_tx = network_error_tx.clone();
                    let print_tx = print_tx.clone();
                    tokio::spawn(async move {
                        match result_rx.await.unwrap_or(Err(NetworkErrorKind::Timeout)) {
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
                                return
                            },
                            Err(e) => {
                                if let NetworkErrorKind::Offline = e {
                                    let _ = peers.write().await.remove(&target);
                                }
                                let _ = network_error_tx
                                    .send(WrappedNetworkError {
                                        id: km.id,
                                        source: km.source,
                                        error: NetworkError {
                                            kind: e,
                                            target: km.target,
                                            message: km.message,
                                            payload: km.payload,
                                        },
                                    })
                                    .await;
                                return
                            }
                        }
                    });
                } else {
                    drop(peers_read);
                    message_to_new_peer(
                        our.clone(),
                        our_ip.clone(),
                        keypair.clone(),
                        pki.clone(),
                        peers.clone(),
                        self_tx.clone(),
                        kernel_message_tx.clone(),
                        network_error_tx.clone(),
                        km,
                    ).await;
                }
            }
        } => Err(anyhow::anyhow!("sender died")),
    }
}

async fn message_to_new_peer(
    our: Identity,
    our_ip: String,
    keypair: Arc<Ed25519KeyPair>,
    pki: OnchainPKI,
    peers: Peers,
    self_tx: MessageSender,
    kernel_message_tx: MessageSender,
    network_error_tx: NetworkErrorSender,
    km: KernelMessage,
) {
    let target = &km.target.node;
    // println!("sending message to unknown peer\r");
    // search PKI for peer and attempt to create a connection, then resend
    let pki_read = pki.read().await;
    match pki_read.get(target) {
        // peer does not exist in PKI!
        None => {
            let _ = network_error_tx
                .send(WrappedNetworkError {
                    id: km.id,
                    source: km.source,
                    error: NetworkError {
                        kind: NetworkErrorKind::Offline,
                        target: km.target,
                        message: km.message,
                        payload: km.payload,
                    },
                })
                .await;
            return;
        }
        // peer exists in PKI
        Some(peer_id) => {
            let peer_id = peer_id.clone();
            drop(pki_read);
            match &peer_id.ws_routing {
                //
                //  can connect directly to peer
                //
                Some((ip, port)) => {
                    if let Ok(ws_url) = make_ws_url(&our_ip, ip, port) {
                        // if we can't connect in 5s, peer is offline
                        if let Ok(Ok((websocket, _response))) =
                            timeout(TIMEOUT, connect_async(ws_url)).await
                        {
                            let conn = build_connection(
                                our.clone(),
                                keypair.clone(),
                                Some(peer_id.clone()),
                                None,
                                pki.clone(),
                                peers.clone(),
                                websocket,
                                kernel_message_tx.clone(),
                            )
                            .await;
                            if !conn.is_err() {
                                // try to resend, now that conn is open
                                let _ = self_tx.send(km).await;
                                return;
                            }
                        }
                    }
                    let _ = network_error_tx
                        .send(WrappedNetworkError {
                            id: km.id,
                            source: km.source,
                            error: NetworkError {
                                kind: NetworkErrorKind::Offline,
                                target: km.target,
                                message: km.message,
                                payload: km.payload,
                            },
                        })
                        .await;
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
                        } else if let Some(router_id) = pki.read().await.get(&router) {
                            if let Some((ip, port)) = &router_id.ws_routing {
                                // println!("trying to connect to {router_name}\r");
                                if let Ok(ws_url) = make_ws_url(&our_ip, ip, port) {
                                    if let Ok(Ok((websocket, _response))) =
                                        timeout(TIMEOUT, connect_async(ws_url)).await
                                    {
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
                    let _ = network_error_tx
                        .send(WrappedNetworkError {
                            id: km.id,
                            source: km.source,
                            error: NetworkError {
                                kind: NetworkErrorKind::Offline,
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
    }
}

/// only used if indirect. should live forever unless we can't connect to any routers
async fn connect_to_routers(
    our: Identity,
    keypair: Arc<Ed25519KeyPair>,
    our_ip: String,
    pki: OnchainPKI,
    peers: Peers,
    kernel_message_tx: MessageSender,
    print_tx: PrintSender,
) {
    let mut routers = JoinSet::<Result<String, tokio::task::JoinError>>::new();

    loop {
        for router_name in &our.allowed_routers {
            if peers.read().await.contains_key(router_name) {
                continue;
            } else if let Some(router_id) = pki.read().await.get(router_name).clone() {
                if let Some((ip, port)) = &router_id.ws_routing {
                    // println!("trying to connect to {router_name}\r");
                    if let Ok(ws_url) = make_ws_url(&our_ip, ip, port) {
                        if let Ok(Ok((websocket, _response))) =
                            timeout(TIMEOUT, connect_async(ws_url)).await
                        {
                            // this is a real and functional router! woohoo
                            if let Ok(active_conn) = build_connection(
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
                                let _ = print_tx
                                    .send(Printout {
                                        verbosity: 0,
                                        content: format!("(re)connected to router: {router_name}"),
                                    })
                                    .await;
                                routers.spawn(active_conn);
                                continue;
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
        if let Some(Ok(Ok(dead_router))) = routers.join_next().await {
            let _ = print_tx
                .send(Printout {
                    verbosity: 0,
                    content: format!("lost connection to router: {dead_router}"),
                })
                .await;
            peers.write().await.remove(&dead_router);
            continue;
        }
        let _ = print_tx
            .send(Printout {
                verbosity: 0,
                content: format!("offline! couldn't connect to any routers. trying again..."),
            })
            .await;
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
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
                    None,
                    None,
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
        // available commands: "peers"
        match data.as_str() {
            "peers" => {
                let peer_read = peers.read().await;
                let _ = print_tx
                    .send(Printout {
                        verbosity: 0,
                        content: format!("{:?}", peer_read.keys()),
                    })
                    .await;
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
}

/*
 *  networking utils
 */

fn make_ws_url(our_ip: &str, ip: &str, port: &u16) -> Result<url::Url, NetworkErrorKind> {
    // if we have the same public IP as target, route locally,
    // otherwise they will appear offline due to loopback stuff
    let ip = if our_ip == ip { "localhost" } else { ip };
    match url::Url::parse(&format!("ws://{}:{}/ws", ip, port)) {
        Ok(v) => Ok(v),
        Err(_) => Err(NetworkErrorKind::Offline),
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
