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
const META_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

type Peers = Arc<RwLock<HashMap<String, Peer>>>;
type WebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type MessageResult = Result<Option<NetworkMessage>, NetworkError>;
type ErrorShuttle = Option<oneshot::Sender<MessageResult>>;

pub struct Peer {
    pub networking_address: String,
    pub is_ward: bool,
    pub sender: mpsc::UnboundedSender<(NetworkMessage, ErrorShuttle)>,
    pub handler: mpsc::UnboundedSender<Vec<u8>>,
    pub destructor: mpsc::UnboundedSender<()>,
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
    Raw(WrappedMessage),
    Handshake(Handshake),
    HandshakeAck(Handshake),
    Error(NetworkError),
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
    init: bool,
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

    let s_keypair = keypair.clone();
    let s_our = our.clone();
    let s_peers = peers.clone();
    let s_our_ip = our_ip.clone();
    let s_pki = pki.clone();
    let s_kernel_message_tx = kernel_message_tx.clone();
    let s_print_tx = print_tx.clone();
    let sender = tokio::spawn(async move {
        while let Some(wm) = message_rx.recv().await {
            if wm.target.node == s_our.name {
                handle_incoming_message(&s_our, wm.message, s_peers.clone(), s_print_tx.clone())
                    .await;
                continue;
            }
            let start = std::time::Instant::now();
            // TODO can we move this into its own task? absolutely..
            let result = timeout(
                META_TIMEOUT,
                message_to_peer(
                    s_our.clone(),
                    s_our_ip.clone(),
                    s_keypair.clone(),
                    s_pki.clone(),
                    s_peers.clone(),
                    wm.clone(),
                    s_kernel_message_tx.clone(),
                ),
            )
            .await;

            let end = std::time::Instant::now();
            let elapsed = end.duration_since(start);
            let _ = s_print_tx
                .send(Printout {
                    verbosity: 1,
                    content: format!("message_to_peer took {:?}", elapsed),
                })
                .await;

            match result {
                Ok(Ok(())) => continue,
                Ok(Err(e)) => {
                    let _ = s_kernel_message_tx
                        .send(make_kernel_response(&s_our, wm, e))
                        .await;
                }
                Err(_) => {
                    let _ = s_kernel_message_tx
                        .send(make_kernel_response(&s_our, wm, NetworkError::Timeout))
                        .await;
                }
            }
        }
    });

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
        _sender = sender => (),
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
    // first accumulate as many connections as possible
    let mut routers = JoinSet::<Result<String, tokio::task::JoinError>>::new();
    for router_name in &our.allowed_routers {
        if let Some(router_id) = pki.read().await.get(router_name) {
            if let Some((ip, port)) = &router_id.ws_routing {
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
                            routers.spawn(active_peer);
                        }
                    }
                }
            }
        }
    }
    // then, poll those connections in parallel
    // if any of them fail, we will try to reconnect
    while let Some(err) = routers.join_next().await {
        if let Ok(Ok(router_name)) = err {
            // try to reconnect
            let _ = print_tx
                .send(Printout {
                    verbosity: 1,
                    content: format!("reconnecting to router: {router_name}\r"),
                })
                .await;
            if let Some(router_id) = pki.read().await.get(&router_name) {
                if let Some((ip, port)) = &router_id.ws_routing {
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
                                routers.spawn(active_peer);
                            }
                        }
                    }
                }
            }
        }
    }
    // if no connections exist, fatal end!
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
    wm: WrappedMessage,
    kernel_message_tx: MessageSender,
) -> Result<(), NetworkError> {
    let target = &wm.target.node;
    let mut peers_write = peers.write().await;
    if let Some(peer) = peers_write.get_mut(target) {
        // if we have the peer, simply send the message to their sender
        let (result_tx, result_rx) = oneshot::channel::<MessageResult>();
        let _ = peer
            .sender
            .send((NetworkMessage::Raw(wm.clone()), Some(result_tx)));
        drop(peers_write);

        match result_rx.await.unwrap_or(Err(NetworkError::Timeout)) {
            Ok(_) => return Ok(()),
            Err(_e) => {
                // if a message to a "known peer" fails, before throwing error,
                // try to reconnect to them, possibly with different routing.
                peers.write().await.remove(target);
            }
        }
    } else {
        drop(peers_write);
    }
    // if peer is a *router*, reconnect will be attempted automatically.
    // TODO: *wait* here until this is *known to have happened*!
    // this solution is only remotely acceptable because of the META_TIMEOUT
    if our.allowed_routers.contains(target) {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        return message_to_peer(
            our,
            our_ip,
            keypair.clone(),
            pki.clone(),
            peers,
            wm,
            kernel_message_tx,
        )
        .await;
    }
    // search PKI for peer and attempt to create a connection, then resend
    match pki.read().await.get(target) {
        // peer does not exist in PKI!
        None => {
            return Err(NetworkError::Offline);
        }
        // peer exists in PKI
        Some(peer_id) => {
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
                                wm,
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
                        let (result_tx, result_rx) = oneshot::channel::<MessageResult>();
                        let res = connections::build_routed_connection(
                            our.clone(),
                            our_ip.clone(),
                            keypair.clone(),
                            router.clone(),
                            (wm.clone(), Some(result_tx)),
                            pki.clone(),
                            peers.clone(),
                            kernel_message_tx.clone(),
                        )
                        .await;
                        if let Ok(Some(new_router)) = res {
                            routers_to_try.push_back(new_router);
                            continue;
                        }
                        if result_rx
                            .await
                            .unwrap_or(Err(NetworkError::Timeout))
                            .is_ok()
                        {
                            return Ok(());
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
    message: Result<Message, UqbarError>,
    peers: Peers,
    print_tx: PrintSender,
) {
    let Ok(message) = message else {
        return;  //  TODO: handle error?
    };

    if message.source.node != our.name {
        let _ = print_tx
            .send(Printout {
                verbosity: 0,
                content: format!(
                    "\x1b[3;32m{}: {}\x1b[0m",
                    message.source.node,
                    message
                        .content
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
            .content
            .payload
            .json
            .as_ref()
            .unwrap_or(&serde_json::Value::Null)
        {
            serde_json::Value::String(s) => {
                if s == "peers" {
                    let peer_read = peers.read().await;
                    let _ = print_tx
                        .send(Printout {
                            verbosity: 0,
                            content: format!("{:?}", peer_read.keys()),
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

fn make_kernel_response(our: &Identity, wm: WrappedMessage, err: NetworkError) -> WrappedMessage {
    WrappedMessage {
        id: wm.id,
        target: ProcessNode {
            node: our.name.clone(),
            process: match wm.message {
                Ok(m) => m.source.process,
                Err(e) => e.source.process,
            },
        },
        rsvp: None,
        message: Ok(Message {
            source: ProcessNode {
                node: our.name.clone(),
                process: "net".into(),
            },
            content: MessageContent {
                message_type: MessageType::Response,
                payload: Payload {
                    json: Some(serde_json::to_value(err).unwrap_or("".into())),
                    bytes: PayloadBytes {
                        circumvent: Circumvent::False,
                        content: None,
                    },
                },
            }
        }),
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
    init: bool,
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
        init,
        id_signature: signed_id,
        ephemeral_public_key: ephemeral_public_key.to_sec1_bytes().to_vec(),
        ephemeral_public_key_signature: signed_pk,
        nonce,
    };

    (ephemeral_secret, handshake)
}
