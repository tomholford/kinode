use std::{collections::HashMap, fmt, sync::Arc};

use crate::types::*;

use aes_gcm::aead::Aead;
use aes_gcm::KeyInit;
use aes_gcm_siv::{Aes256GcmSiv, Nonce};
use elliptic_curve::ecdh::EphemeralSecret;
use elliptic_curve::PublicKey;
use ethers::prelude::k256::Secp256k1;
use futures::future::Join;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use ring::signature::{self, Ed25519KeyPair};
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, RwLock};
use tokio::task::{AbortHandle, JoinHandle, JoinSet};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::{self};
use tokio_tungstenite::{accept_async, connect_async, MaybeTlsStream, WebSocketStream};

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

type Peers = Arc<RwLock<HashMap<String, Peer>>>;
type WebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Peer {
    pub identity: Identity,
    pub direct: bool,
    pub is_ward: bool,
    pub websocket: WebSocket,
}

/// parsed from Binary websocket message on an Indirect route
#[derive(Debug, Serialize, Deserialize)]
enum NetworkMessage {
    From { from: String, contents: Vec<u8> },
    To { to: String, contents: Vec<u8> },
    Handshake(Handshake),
    LostPeer(String),
    PeerOffline(String),
}

#[derive(Debug, Serialize, Deserialize)]
enum NetworkError {
    Timeout,
    Offline,
}

/// contains identity and encryption keys, used in initial handshake.
/// parsed from Text websocket message
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Handshake {
    from: String,
    target: String,
    routing_request: bool,
    id_signature: Vec<u8>,
    ephemeral_public_key: Vec<u8>,
    ephemeral_public_key_signature: Vec<u8>,
    nonce: Option<Vec<u8>>,
}

pub async fn networking(
    our: Identity,
    our_ip: String,
    keypair: Ed25519KeyPair,
    pki: OnchainPKI,
    kernel_message_tx: MessageSender,
    print_tx: PrintSender,
    mut message_rx: MessageReceiver,
    self_message_tx: MessageSender,
) {
    // if we do routing for a node, they're stored here
    let peers: Peers = Arc::new(RwLock::new(HashMap::new()));
    let keypair = Arc::new(keypair);

    let listener_handle = match &our.ws_routing {
        None => {
            // connect to router(s)
            tokio::spawn(connect_to_routers(
                our.clone(),
                our_ip.clone(),
                pki.clone(),
                kernel_message_tx.clone(),
            ))
        }
        Some((_ip, port)) => {
            // spawn the listener
            tokio::spawn(receive_incoming_connections(
                our.clone(),
                *port,
                pki.clone(),
                peers.clone(),
                kernel_message_tx.clone(),
            ))
        }
    };

    tokio::select! {
        _listener = listener_handle => (),
        _sender = async {
            while let Some(wm) = message_rx.recv().await {
                if wm.message.wire.target_ship == our.name {
                    handle_incoming_message(&our, wm.message, peers.clone(), print_tx.clone()).await;
                    continue;
                }
                tokio::spawn(message_to_peer(
                    our.clone(),
                    our_ip.clone(),
                    keypair.clone(),
                    pki.clone(),
                    peers.clone(),
                    wm,
                    print_tx.clone(),
                    kernel_message_tx.clone(),
                    self_message_tx.clone(),
                ));
            }
        } => (),
    }
}

/// only used if indirect. should live forever unless we can't connect to any routers
async fn connect_to_routers(
    our: Identity,
    our_ip: String,
    pki: OnchainPKI,
    kernel_message_tx: MessageSender,
) {
    // first accumulate as many connections as possible
    let mut routers = JoinSet::<()>::new();
    for router_name in &our.allowed_routers {
        println!("trying for router {}", router_name);
        if let Some(router_id) = pki.read().await.get(router_name) {
            if let Some((ip, port)) = &router_id.ws_routing {
                if let Ok(ws_url) = make_ws_url(&our_ip, ip, port) {
                    if let Ok((mut websocket, _response)) = connect_async(ws_url).await {
                        // TODO establish routership with handshake protocol etc.
                        let _ = websocket
                            .send(tungstenite::Message::Text(our.name.to_string()))
                            .await;
                        // this is a real and functional router! woohoo
                        println!("connected to router {}!", router_name);
                        routers.spawn(router_connection(
                            our.clone(),
                            websocket,
                            kernel_message_tx.clone(),
                        ));
                    }
                }
            }
        }
    }
    println!("here now");
    // then, poll those connections in parallel
    while let Some(err) = routers.join_next().await {
        println!("lost a router! {:?}", err);
    }
    println!("oh no");
    // if no connections exist, fatal end
}

async fn router_connection(
    our: Identity,
    mut websocket: WebSocket,
    kernel_message_tx: MessageSender,
) {
    // TODO: KEEP THIS SOCKET ALIVE!
    while let Some(msg) = websocket.next().await {
        println!("got msg on router_connection!");
        if let Ok(tungstenite::Message::Binary(bin)) = msg {
            let msg = serde_json::from_slice::<WrappedMessage>(&bin).unwrap();
            if msg.message.wire.target_ship == our.name {
                let _ = websocket.send(tungstenite::Message::Pong(vec![])).await;
                let _ = kernel_message_tx.send(msg).await;
            }
        }
    }
}

/// only used if direct. should live forever
async fn receive_incoming_connections(
    our: Identity,
    port: u16,
    pki: OnchainPKI,
    peers: Peers,
    kernel_message_tx: MessageSender,
) {
    let tcp = TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .expect(format!("fatal error: can't listen on port {}", port).as_str());

    while let Ok((stream, _socket_addr)) = tcp.accept().await {
        match accept_async(MaybeTlsStream::Plain(stream)).await {
            Ok(websocket) => {
                tokio::spawn(handle_incoming_connection(
                    our.clone(),
                    websocket,
                    pki.clone(),
                    peers.clone(),
                    kernel_message_tx.clone(),
                ));
            }
            // ignore connections we failed to accept
            Err(_) => {}
        }
    }
}

async fn handle_incoming_connection(
    our: Identity,
    mut websocket: WebSocket,
    pki: OnchainPKI,
    peers: Peers,
    kernel_message_tx: MessageSender,
) {
    // receive message
    // if target is us:
    //    send pong with message id
    //    close if get nothing within timeout
    // otherwise:
    //    decide if we can or should route to them
    //    if we want to, route to them
    //    only pong original sender once we get pong from routed-to
    //
    if let Ok(Some(Ok(ws_message))) = timeout(TIMEOUT, websocket.next()).await {
        match ws_message {
            tungstenite::Message::Text(msg) => {
                // this is a networking protocol message
                // TODO do more than assume it's a request for routing
                let their_name = msg;
                if let Some(their_id) = pki.read().await.get(&their_name) {
                    let ward_peer = Peer {
                        identity: their_id.clone(),
                        direct: true,
                        is_ward: true,
                        websocket,
                    };
                    peers.write().await.insert(their_name, ward_peer);
                    return;
                }
            }
            tungstenite::Message::Binary(bin) => {
                let msg = serde_json::from_slice::<WrappedMessage>(&bin).unwrap();
                if msg.message.wire.target_ship == our.name {
                    let _ = websocket.send(tungstenite::Message::Pong(vec![])).await;
                    let _ = kernel_message_tx.send(msg).await;
                } else {
                    if let Some(peer) = peers.write().await.get_mut(&msg.message.wire.target_ship) {
                        match send_routed_message(msg, peer).await {
                            Ok(()) => {
                                let _ = websocket.send(tungstenite::Message::Pong(vec![])).await;
                            }
                            Err(_) => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

async fn message_to_peer(
    our: Identity,
    our_ip: String,
    keypair: Arc<Ed25519KeyPair>,
    pki: OnchainPKI,
    peers: Peers,
    wm: WrappedMessage,
    print_tx: PrintSender,
    kernel_message_tx: MessageSender,
    self_message_tx: MessageSender,
) {
    let target = &wm.message.wire.target_ship;
    match peers.write().await.get_mut(target) {
        Some(peer) => match send_routed_message(wm.clone(), peer).await {
            Ok(()) => return,
            Err(e) => {
                let _ = kernel_message_tx
                    .send(make_kernel_response(&our, wm, e))
                    .await;
                return;
            }
        },
        None => {
            // search PKI for peer and attempt to create a connection, then resend
            match pki.read().await.get(target) {
                None => {
                    // peer does not exist in PKI!
                    let _ = kernel_message_tx
                        .send(make_kernel_response(&our, wm, NetworkError::Offline))
                        .await;
                }
                Some(peer_id) => {
                    match &peer_id.ws_routing {
                        Some((ip, port)) => {
                            //
                            //  can connect directly to peer
                            //
                            match send_direct_message(&our, &our_ip, wm.clone(), ip, port).await {
                                Ok(()) => return,
                                Err(e) => {
                                    let _ = kernel_message_tx
                                        .send(make_kernel_response(&our, wm, e))
                                        .await;
                                    return;
                                }
                            }
                        }
                        None => {
                            //
                            //  peer does not have direct routing info, need to use router
                            //
                            for router in &peer_id.allowed_routers {
                                if let Some(router_id) = pki.read().await.get(router) {
                                    if let Some((ip, port)) = &router_id.ws_routing {
                                        match send_direct_message(
                                            &our,
                                            &our_ip,
                                            wm.clone(),
                                            ip,
                                            port,
                                        )
                                        .await
                                        {
                                            Ok(()) => return,
                                            Err(_) => continue,
                                        }
                                    }
                                }
                            }
                            // we tried all available routers and none of them worked!
                            let _ = kernel_message_tx
                                .send(make_kernel_response(&our, wm, NetworkError::Offline))
                                .await;
                            return;
                        }
                    }
                }
            }
        }
    }
}

async fn send_direct_message(
    our: &Identity,
    our_ip: &String,
    wm: WrappedMessage,
    ip: &str,
    port: &u16,
) -> Result<(), NetworkError> {
    //
    //  can make direct connection to peer
    //
    if let Ok(ws_url) = make_ws_url(&our_ip, ip, port) {
        // TODO *optimize* by keeping this websocket OPEN and reusing it!!
        if let Ok((mut websocket, _response)) = connect_async(ws_url).await {
            let _ = websocket
                .send(tungstenite::Message::Binary(
                    serde_json::to_vec(&wm).unwrap(),
                ))
                .await;
            // either we get pong, or we get timeout
            if let Ok(sent) = timeout(TIMEOUT, websocket.next()).await {
                match sent {
                    None => {
                        // what does this mean?
                        return Err(NetworkError::Offline);
                    }
                    Some(Err(e)) => {
                        // what does this mean?
                        // could be different depending on error
                        // can treat as offline
                        return Err(NetworkError::Offline);
                    }
                    Some(Ok(tungstenite::Message::Pong(_))) => {
                        // woohoo success
                        return Ok(());
                    }
                    _ => {
                        // got response other than Pong
                        return Err(NetworkError::Timeout);
                    }
                }
            }
            // timeout
            return Err(NetworkError::Timeout);
        }
        // offline, couldn't connect to given URL
    }
    // offline, couldn't make URL
    Err(NetworkError::Offline)
}

async fn send_routed_message(wm: WrappedMessage, peer: &mut Peer) -> Result<(), NetworkError> {
    if peer.is_ward {
        let _ = peer
            .websocket
            .send(tungstenite::Message::Binary(
                serde_json::to_vec(&wm).unwrap(),
            ))
            .await;
        // either we get pong, or we get timeout
        if let Ok(sent) = timeout(TIMEOUT, peer.websocket.next()).await {
            match sent {
                None => {
                    // what does this mean?
                    return Err(NetworkError::Offline);
                }
                Some(Err(_e)) => {
                    // what does this mean?
                    // could be different depending on error
                    // can treat as offline
                    return Err(NetworkError::Offline);
                }
                Some(Ok(tungstenite::Message::Pong(_))) => {
                    // woohoo success
                    return Ok(());
                }
                _ => {
                    // got response other than Pong
                    return Err(NetworkError::Timeout);
                }
            }
        }
        // timeout
        return Err(NetworkError::Timeout);
    }
    Err(NetworkError::Offline)
}

async fn handle_incoming_message(
    our: &Identity,
    message: Message,
    peers: Peers,
    print_tx: PrintSender,
) {
    if message.wire.source_ship != our.name {
        let _ = print_tx
            .send(Printout {
                verbosity: 0,
                content: format!(
                    "\x1b[3;32m{}: {}\x1b[0m",
                    message.wire.source_ship,
                    message
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
        rsvp: None,
        message: Message {
            message_type: MessageType::Response,
            wire: Wire {
                source_ship: our.name.clone(),
                source_app: "net".into(),
                target_ship: our.name.clone(),
                target_app: wm.message.wire.source_app,
            },
            payload: Payload {
                json: Some(serde_json::to_value(err).unwrap()),
                bytes: None,
            },
        },
    }
}
