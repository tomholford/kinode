use std::collections::VecDeque;
use std::{collections::HashMap, sync::Arc};

use crate::net::connections::build_connection;
use crate::types::*;

use aes_gcm_siv::Nonce;
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
// const META_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

type Peers = Arc<RwLock<HashMap<String, Peer>>>;
type WebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type MessageResult = Result<Option<NetworkMessage>, NetworkError>;
type ErrorShuttle = Option<oneshot::Sender<MessageResult>>;

#[derive(Clone)]
pub struct Peer {
    pub networking_address: String,
    pub sender: mpsc::UnboundedSender<(NetworkMessage, ErrorShuttle)>,
    pub handler: mpsc::UnboundedSender<Vec<u8>>,
    pub destructor: mpsc::UnboundedSender<()>,
}

/// parsed from Binary websocket message on an Indirect route
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NetworkMessage {
    Ack(u64),
    Nack(u64),
    Keepalive,
    Msg {
        from: String,
        to: String,
        id: u64,
        contents: Vec<u8>,
    },
    Raw(KernelMessage),
    Handshake {
        id: u64,
        handshake: Handshake,
    },
    HandshakeAck {
        id: u64,
        handshake: Handshake,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NetworkError {
    Timeout,
    Offline,
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
    keypair: Ed25519KeyPair,
    pki: OnchainPKI,
    kernel_message_tx: MessageSender,
    print_tx: PrintSender,
    mut message_rx: MessageReceiver,
) {
    let peers: Peers = Arc::new(RwLock::new(HashMap::new()));
    let keypair = Arc::new(keypair);

    tokio::select! {
        _listener = async {
            // if listener dies, attempt to rebuild it
            loop {
                match &our.ws_routing {
                    None => {
                        // connect to router(s)
                        connect_to_routers(
                            our.clone(),
                            keypair.clone(),
                            our_ip.clone(),
                            pki.clone(),
                            peers.clone(),
                            kernel_message_tx.clone(),
                            print_tx.clone(),
                        ).await;
                    }
                    Some((_ip, port)) => {
                        // spawn the listener
                        receive_incoming_connections(
                            our.clone(),
                            keypair.clone(),
                            *port,
                            pki.clone(),
                            peers.clone(),
                            kernel_message_tx.clone(),
                        ).await;
                    }
                }
                let _ = print_tx.send(Printout {
                    verbosity: 0,
                    content: "network connection died, attempting to regain...".into()
                }).await;
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        } => (),
        _sender = async {
            while let Some(km) = message_rx.recv().await {
                tokio::spawn(sender(
                    our.clone(),
                    our_ip.clone(),
                    keypair.clone(),
                    pki.clone(),
                    peers.clone(),
                    kernel_message_tx.clone(),
                    print_tx.clone(),
                    km,
                ));
            }
        } => (),
    }
}

async fn sender(
    our: Identity,
    our_ip: String,
    keypair: Arc<Ed25519KeyPair>,
    pki: OnchainPKI,
    peers: Peers,
    kernel_message_tx: MessageSender,
    print_tx: PrintSender,
    km: KernelMessage,
) {
    if km.target.node == our.name {
        handle_incoming_message(&our, km.message, peers.clone(), print_tx.clone()).await;
        return;
    }
    let start = std::time::Instant::now();
    let result = message_to_peer(
        our.clone(),
        our_ip.clone(),
        keypair.clone(),
        pki.clone(),
        peers.clone(),
        km.clone(),
        kernel_message_tx.clone(),
    )
    .await;
    let end = std::time::Instant::now();
    let elapsed = end.duration_since(start);
    let _ = print_tx
        .send(Printout {
            verbosity: 0,
            content: format!(
                "message to {} took {:?} (result: {:?})",
                km.target.node, elapsed, result
            ),
        })
        .await;

    match result {
        Ok(()) => return,
        Err(e) => {
            let _ = kernel_message_tx
                .send(make_kernel_response(&our, km, e))
                .await;
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
                            if let Ok(active_peer) = build_connection(
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
                                routers.spawn(active_peer);
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
        break;
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
                tokio::spawn(connections::build_connection(
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

#[async_recursion]
async fn message_to_peer(
    our: Identity,
    our_ip: String,
    keypair: Arc<Ed25519KeyPair>,
    pki: OnchainPKI,
    peers: Peers,
    km: KernelMessage,
    kernel_message_tx: MessageSender,
) -> Result<(), NetworkError> {
    let target = &km.target.node;
    let mut peers_write = peers.write().await;
    if let Some(peer) = peers_write.get_mut(target) {
        // println!("sending message to known peer\r");
        // if we have the peer, simply send the message to their sender
        let (result_tx, result_rx) = oneshot::channel::<MessageResult>();
        let _ = peer
            .sender
            .send((NetworkMessage::Raw(km.clone()), Some(result_tx)));
        drop(peers_write);

        match result_rx.await.unwrap_or(Err(NetworkError::Timeout)) {
            Ok(_) => return Ok(()),
            Err(e) => {
                // if a message to a "known peer" fails, before throwing error,
                // try to reconnect to them, possibly with different routing.
                // unless known peer is a router, in which case reconnect
                // will happen automatically.
                if our.allowed_routers.contains(target) {
                    return Err(e);
                }
                peers.write().await.remove(target);
            }
        }
    } else {
        drop(peers_write);
    }
    // println!("sending message to unknown peer\r");
    // search PKI for peer and attempt to create a connection, then resend
    let pki_read = pki.read().await;
    match pki_read.get(target) {
        // peer does not exist in PKI!
        None => {
            return Err(NetworkError::Offline);
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
                            let conn = connections::build_connection(
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
                            if conn.is_err() {
                                return Err(NetworkError::Offline);
                            }
                            return message_to_peer(
                                our,
                                our_ip,
                                keypair.clone(),
                                pki.clone(),
                                peers,
                                km,
                                kernel_message_tx,
                            )
                            .await;
                        }
                    }
                    return Err(NetworkError::Offline);
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
                        let (result_tx, result_rx) = oneshot::channel::<MessageResult>();
                        let res = connections::build_routed_connection(
                            our.clone(),
                            our_ip.clone(),
                            keypair.clone(),
                            router.clone(),
                            (km.clone(), Some(result_tx)),
                            pki.clone(),
                            peers.clone(),
                            kernel_message_tx.clone(),
                        )
                        .await;
                        if let Ok(()) = res {
                            if result_rx
                                .await
                                .unwrap_or(Err(NetworkError::Timeout))
                                .is_ok()
                            {
                                return Ok(());
                            }
                        }
                    }
                    //
                    // we tried all available routers and none of them worked!
                    //
                    return Err(NetworkError::Offline);
                }
            }
        }
    }
}

async fn handle_incoming_message(
    our: &Identity,
    message: Result<TransitMessage, UqbarError>,
    peers: Peers,
    print_tx: PrintSender,
) {
    let Ok(message) = message else {
        return;  //  TODO: handle error?
    };

    let payload = match message {
        TransitMessage::Request(request) => request.payload,
        TransitMessage::Response(payload) => payload,
    };

    if payload.source.node != our.name {
        let _ = print_tx
            .send(Printout {
                verbosity: 0,
                content: format!(
                    "\x1b[3;32m{}: {}\x1b[0m",
                    payload.source.node,
                    // payload.json,
                    payload.json.as_ref().unwrap_or(&"".to_string()),
                ),
            })
            .await;
    } else {
        // available commands: peers
        match payload.json.unwrap_or("".to_string()).as_str() {
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

fn make_ws_url(our_ip: &str, ip: &str, port: &u16) -> Result<url::Url, NetworkingError> {
    // if we have the same public IP as target, route locally,
    // otherwise they will appear offline due to loopback stuff
    let ip = if our_ip == ip { "localhost" } else { ip };
    match url::Url::parse(&format!("ws://{}:{}/ws", ip, port)) {
        Ok(v) => Ok(v),
        Err(_) => Err(NetworkingError::PeerOffline),
    }
}

fn make_kernel_response(our: &Identity, km: KernelMessage, err: NetworkError) -> KernelMessage {
    KernelMessage {
        id: km.id,
        target: ProcessReference {
            node: our.name.clone(),
            identifier: match km.message {
                Err(e) => e.source.identifier,
                Ok(m) => match m {
                    TransitMessage::Request(request) => request.payload.source.identifier,
                    TransitMessage::Response(payload) => payload.source.identifier,
                },
            },
        },
        rsvp: None,
        message: Ok(TransitMessage::Response(TransitPayload {
            source: ProcessReference {
                node: our.name.clone(),
                identifier: ProcessIdentifier::Name("net".into()),
            },
            json: Some(serde_json::to_string(&err).unwrap_or("".into())),
            bytes: TransitPayloadBytes::None,
        })),
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
