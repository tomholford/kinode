use aes_gcm_siv::{
    aead::{Aead, KeyInit},
    Aes256GcmSiv, Nonce,
};
use elliptic_curve::ecdh::EphemeralSecret;
use elliptic_curve::PublicKey;
use ethers::prelude::k256::Secp256k1;
use futures::prelude::*;
use futures::stream::{SplitSink, SplitStream};
use ring::signature::{self, Ed25519KeyPair};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::{accept_async, connect_async, tungstenite::Result};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use url::Url;

use crate::types::*;
use ethers::prelude::*;

pub struct Peer {
    pub address: H256,
    pub ws_url: Url,
    pub nonce: Arc<Nonce>,
    pub our_ephemeral_secret: Arc<EphemeralSecret<Secp256k1>>,
    pub their_ephemeral_pk: Arc<PublicKey<Secp256k1>>,
    pub ws_write_stream: SplitSink<Sock, tungstenite::Message>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SignedMessage {
    signature: Vec<u8>,
    message: Message,
}

/// contains identity and encryption keys, used in initial handshake
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Handshake {
    from: (String, String),
    target: String,
    id_signature: Vec<u8>,
    ephemeral_public_key: Vec<u8>,
    ephemeral_public_key_signature: Vec<u8>,
    nonce: Option<Vec<u8>>,
}

type Peers = Arc<RwLock<HashMap<String, Peer>>>;
type Sock = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Clone)]
enum HandshakeOrTarget {
    Handshake(Handshake),
    Target(String),
}

enum SuccessOrTimeout {
    Success,
    Timeout,
    TryAgain,
}

/// websockets driver.
///
/// statelessly manages websocket connections to peer nodes
pub async fn websockets(
    our: Identity,
    keypair: Ed25519KeyPair,
    pki: OnchainPKI,
    message_rx: MessageReceiver,
    self_message_tx: MessageSender,
    message_tx: MessageSender,
    print_tx: PrintSender,
) {
    let our_port: u16 = match our.ws_routing {
        Some((_, port)) => port,
        None => 9999, // TODO make this configurable without being onchain
    };
    let tcp_listener = TcpListener::bind(format!("0.0.0.0:{}", our_port))
        .await
        .expect(format!("error: can't listen on port {}", our_port).as_str());

    // initialize peer-connection-map as empty -- can pre-populate as optimization?
    let peers: Peers = Arc::new(RwLock::new(HashMap::new()));

    let _ = print_tx
        .send(format!("now listening on port {}", our_port))
        .await;

    let keypair = Arc::new(keypair);

    let our_url = Arc::new(Url::parse(&format!("ws://0.0.0.0:{}/ws", our_port)).unwrap());

    // listen on our port for new connections, and
    // listen on our receiver for messages to send to peers
    tokio::join!(
        ws_listener(
            our.clone(),
            our_url.clone(),
            keypair.clone(),
            pki.clone(),
            tcp_listener,
            peers.clone(),
            message_tx.clone(),
            print_tx.clone(),
        ),
        ws_sender(
            our.clone(),
            our_url.clone(),
            keypair.clone(),
            pki.clone(),
            peers.clone(),
            message_rx,
            self_message_tx.clone(),
            message_tx.clone(),
            print_tx.clone(),
        ),
    );
}

/// listen for new connections on our port, spawn a new task for each one
/// that will call handle_connection()
async fn ws_listener(
    our: Identity,
    our_url: Arc<Url>,
    keypair: Arc<Ed25519KeyPair>,
    pki: OnchainPKI,
    tcp: TcpListener,
    peers: Peers,
    message_tx: MessageSender,
    print_tx: PrintSender,
) {
    while let Ok((stream, _socket_addr)) = tcp.accept().await {
        let stream = accept_async(MaybeTlsStream::Plain(stream)).await;
        match stream {
            Ok(stream) => {
                let conn = establish_route(
                    our.clone(),
                    our_url.clone(),
                    keypair.clone(),
                    pki.clone(),
                    stream,
                    peers.clone(),
                    message_tx.clone(),
                    print_tx.clone(),
                )
                .await;
                match conn {
                    Ok(_) => {}
                    Err(e) => {
                        let _ = print_tx
                            .send(format!("peer connection failed: {}", e))
                            .await;
                    }
                }
            }
            Err(e) => {
                let _ = print_tx
                    .send(format!("peer connection failed: {}", e))
                    .await;
            }
        }
    }
}

/// respond to messages received from kernel, either by sending on open connection
/// or spawning a new connection using handle_connection()
pub async fn ws_sender(
    our: Identity,
    our_url: Arc<Url>,
    keypair: Arc<Ed25519KeyPair>,
    pki: OnchainPKI,
    peers: Peers,
    mut message_rx: MessageReceiver,
    self_message_tx: MessageSender,
    kernel_message_tx: MessageSender,
    print_tx: PrintSender,
) {
    while let Some(message_stack) = message_rx.recv().await {
        let stack_len = message_stack.len();
        let message = &message_stack[stack_len - 1];

        let result = send_message(
            &our,
            our_url.clone(),
            keypair.clone(),
            pki.clone(),
            peers.clone(),
            message.clone(),
            self_message_tx.clone(),
            kernel_message_tx.clone(),
            print_tx.clone(),
        )
        .await;

        match result {
            Ok(res) => {
                if let SuccessOrTimeout::Timeout = res {
                    let _ = kernel_message_tx
                        .send(vec![Message {
                            message_type: MessageType::Response,
                            wire: Wire {
                                source_ship: our.name.clone(),
                                source_app: "ws".into(),
                                target_ship: our.name.clone(),
                                target_app: message.wire.source_app.clone(),
                            },
                            payload: Payload {
                                json: Some(serde_json::to_value(NetworkingError::MessageTimeout).unwrap()),
                                bytes: None,
                            },
                        }])
                        .await;
                }
                continue;
            }
            Err(e) => {
                let _ = print_tx.send(format!("{}", e)).await;
                let _ = kernel_message_tx
                    .send(vec![Message {
                        message_type: MessageType::Response,
                        wire: Wire {
                            source_ship: our.name.to_string(),
                            source_app: "ws".into(),
                            target_ship: our.name.clone(),
                            target_app: message.wire.source_app.clone(),
                        },
                        payload: Payload {
                            json: Some(serde_json::to_value(NetworkingError::PeerOffline).unwrap()),
                            bytes: None,
                        },
                    }])
                    .await;
            }
        }
    }
}

async fn send_message(
    our: &Identity,
    our_url: Arc<Url>,
    keypair: Arc<Ed25519KeyPair>,
    pki: OnchainPKI,
    peers: Peers,
    message: Message,
    self_message_tx: MessageSender,
    kernel_message_tx: MessageSender,
    print_tx: PrintSender,
) -> Result<SuccessOrTimeout, String> {
    let message_bytes = match serde_json::to_vec(&message) {
        Ok(v) => v,
        Err(e) => return Err(format!("error serializing message: {}", e)),
    };

    let mut edit = peers.write().await;
    match edit.get_mut(&message.wire.target_ship) {
        Some(peer) => {
            // we have an active connection with this peer, encrypt and send message
            let encryption_key = peer
                .our_ephemeral_secret
                .diffie_hellman(&peer.their_ephemeral_pk);
            let cipher = Aes256GcmSiv::new(&encryption_key.raw_secret_bytes());
            let ciphertext = match cipher.encrypt(&peer.nonce, message_bytes.as_ref()) {
                Ok(v) => v,
                Err(e) => return Err(format!("error encrypting message: {}", e)),
            };
            // use existing write stream to send message
            match peer
                .ws_write_stream
                .send(tungstenite::Message::Binary(ciphertext))
                .await
            {
                Ok(_) => return Ok(SuccessOrTimeout::Success),
                Err(_) => {
                    //  TODO handle this actual websocket error better
                    edit.remove(&message.wire.target_ship);
                    return Ok(SuccessOrTimeout::Timeout)
                }
            }
        }
        None => {
            // we do not have an active connection, try to open a new connection
            // then re-send the original message
            drop(edit);
            let id = match pki.get(&message.wire.target_ship) {
                Some(v) => v,
                None => return Err(format!("error: {} not in PKI", &message.wire.target_ship)),
            };
            // if their identity does not include direct websocket
            // routing info, try to go through a router
            let ws_url: Url = match &id.ws_routing {
                Some((url, port)) => Url::parse(&format!("ws://{}:{}/ws", url, port)).unwrap(),
                None => {
                    let router = match id.allowed_routers.get(0) {
                        Some(v) => v,
                        None => {
                            return Err(format!(
                                "error: {} has no allowed routers",
                                &message.wire.target_ship
                            ))
                        }
                    };
                    let router_id = match pki.get(router) {
                        Some(v) => v,
                        None => return Err(format!("error: router {} not in PKI", router)),
                    };
                    let (ws_url, ws_port) = match &router_id.ws_routing {
                        Some((url, port)) => (url, port),
                        None => {
                            return Err(format!("error: router {} has no routing info", router))
                        }
                    };
                    Url::parse(&format!("ws://{}:{}/ws", ws_url, ws_port)).unwrap()
                }
            };
            match connect_async(ws_url).await {
                Err(e) => {
                    return Err(format!(
                        "error connecting to {}: {}",
                        &message.wire.target_ship, e
                    ));
                    // try again to send message?
                    // TODO route this back through kernel or something?
                }
                Ok((stream, _response)) => {
                    let conn = handle_connection(
                        our.clone(),
                        our_url.clone(),
                        HandshakeOrTarget::Target(message.wire.target_ship.clone()),
                        keypair.clone(),
                        pki.clone(),
                        stream,
                        peers.clone(),
                        kernel_message_tx.clone(),
                        print_tx.clone(),
                        true, // we are initiator
                    )
                    .await;
                    match conn {
                        Ok(_) => {
                            let _ = self_message_tx.send(vec![message.clone()]).await;
                            return Ok(SuccessOrTimeout::TryAgain);
                        }
                        Err(e) => return Err(format!("error opening new conn: {}", e)),
                    }
                }
            }
        }
    }
}

/// determine if incoming connection is targeting us or if we should
/// route it ahead to a true target.
async fn establish_route(
    our: Identity,
    our_url: Arc<Url>,
    keypair: Arc<Ed25519KeyPair>,
    pki: OnchainPKI,
    mut ws_stream: Sock,
    peers: Peers,
    message_tx: MessageSender,
    print_tx: PrintSender,
) -> Result<(), String> {
    // receive handshake from peer
    let handshake: Handshake = serde_json::from_str(
        &ws_stream
            .next()
            .await
            .ok_or("handshake failed")?
            .map_err(|_| "handshake failed")?
            .into_text()
            .map_err(|_| "got bad handshake")?,
    )
    .map_err(|_| "got bad handshake")?;

    if handshake.target == our.name {
        return handle_connection(
            our,
            our_url.clone(),
            HandshakeOrTarget::Handshake(handshake),
            keypair,
            pki,
            ws_stream,
            peers,
            message_tx,
            print_tx,
            false,
        )
        .await;
    }
    // if the handshake is to be routed, attempt to connect to
    // routing target and forward the handshake to them
    // NB: in order to route for a node, they *must be a peer*,
    // in other words, we must have an active connection with them.
    let ws_url: Url = peers
        .read()
        .await
        .get(&handshake.target)
        .ok_or("router: target not in peer-set")?
        .ws_url
        .clone();

    match connect_async(ws_url).await {
        Err(e) => {
            let _ = print_tx
                .send(format!(
                    "router: error connecting to {}: {}",
                    handshake.target, e
                ))
                .await;
            // try again to send message?
            // TODO route this back through kernel or something?
            Err("router: couldn't connect to target".into())
        }
        Ok((mut ws_stream_2, _response)) => {
            // send the handshake through to target
            ws_stream_2
                .send(tungstenite::Message::Text(
                    serde_json::to_string(&handshake)
                        .map_err(|_| "failed to serialize handshake")?,
                ))
                .await
                .map_err(|_| "failed to send handshake")?;
            tokio::spawn(active_router(ws_stream, ws_stream_2, print_tx));
            Ok(())
        }
    }
}

/// perform two-way handshake with new peer, then start reading from their stream.
/// if connection is closed, remove peer from peer map.
/// this function will live as long as the connection is open.
/// TODO make a good Error type for this
async fn handle_connection(
    our: Identity,
    our_url: Arc<Url>,
    target: HandshakeOrTarget,
    keypair: Arc<Ed25519KeyPair>,
    pki: OnchainPKI,
    ws_stream: Sock,
    peers: Peers,
    message_tx: MessageSender,
    print_tx: PrintSender,
    initiating: bool,
) -> Result<(), String> {
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
        .sign(&serde_json::to_vec(&our).map_err(|_| "failed to serialize identity")?)
        .as_ref()
        .to_vec();

    let nonce = match initiating {
        false => None,
        true => {
            let mut iv = [0u8; 12];
            rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut iv);
            Some(iv.to_vec())
        }
    };

    let signed_identity = Handshake {
        from: (our.name.clone(), our_url.to_string()),
        target: match target.clone() {
            HandshakeOrTarget::Handshake(h) => h.target,
            HandshakeOrTarget::Target(t) => t,
        },
        id_signature: signed_id,
        ephemeral_public_key: ephemeral_public_key.to_sec1_bytes().to_vec(),
        ephemeral_public_key_signature: signed_pk,
        // if we are connection initiator, send nonce inside message
        nonce: nonce.clone(),
    };

    // place the connection, split in two, inside peer mapping
    let (mut write_stream, mut read_stream) = ws_stream.split();

    // take first message on stream and use it to identify peer
    // TODO can try reading multiple messages until a valid handshake is received
    // simultaneously, send our own handshake message
    let handshake_read = read_stream.next();
    let handshake_write = write_stream.send(tungstenite::Message::Text(
        serde_json::to_string(&signed_identity).map_err(|_| "failed to serialize handshake")?,
    ));

    let handshake = match target {
        HandshakeOrTarget::Handshake(h) => {
            let _ = handshake_write.await;
            h
        }
        HandshakeOrTarget::Target(_) => {
            let (got, _) = tokio::join!(handshake_read, handshake_write);
            serde_json::from_str(
                &got.ok_or("handshake failed")?
                    .map_err(|_| "handshake failed")?
                    .into_text()
                    .map_err(|_| "got bad handshake")?,
            )
            .map_err(|_| "got bad handshake")?
        }
    };

    // verify their identity using signatures and pki info
    let their_id: Identity = pki
        .get(&handshake.from.0)
        .ok_or("got handshake from user not in PKI")?
        .clone();

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
    let nonce: Arc<Nonce> = match initiating {
        true => Arc::new(*Nonce::from_slice(&nonce.ok_or("produced bad nonce")?)),
        false => Arc::new(*Nonce::from_slice(&handshake.nonce.ok_or("got bad nonce")?)),
    };

    let _ = print_tx
        .send(format!("shook hands with peer {}", their_id.name))
        .await;

    // add them to in-mem active peer mapping
    peers.write().await.insert(
        their_id.name.clone(),
        Peer {
            address: their_id.address,
            ws_url: Url::parse(&handshake.from.1).unwrap(),
            nonce: nonce.clone(),
            our_ephemeral_secret: ephemeral_secret.clone(),
            their_ephemeral_pk: their_ephemeral_pk.clone(),
            ws_write_stream: write_stream,
        },
    );

    tokio::spawn(active_reader(
        their_id.clone(),
        peers.clone(),
        read_stream,
        ephemeral_secret,
        their_ephemeral_pk,
        nonce,
        message_tx.clone(),
        print_tx.clone(),
    ));

    Ok(())
}

async fn active_reader(
    who: Identity,
    peers: Peers,
    mut read_stream: SplitStream<Sock>,
    ephemeral_secret: Arc<EphemeralSecret<Secp256k1>>,
    their_ephemeral_pk: Arc<PublicKey<Secp256k1>>,
    nonce: Arc<Nonce>,
    message_tx: MessageSender,
    print_tx: PrintSender,
) {
    while let Some(msg) = read_stream.next().await {
        match msg {
            Ok(msg) => {
                // decrypt message
                let encryption_key = ephemeral_secret.diffie_hellman(&their_ephemeral_pk);
                let cipher = Aes256GcmSiv::new(&encryption_key.raw_secret_bytes());
                let plaintext = match cipher.decrypt(&nonce, msg.into_data().as_ref()) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = print_tx
                            .send(format!("error decrypting message: {}", e))
                            .await;
                        continue;
                    }
                };
                let message = match serde_json::from_slice::<Message>(&plaintext) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = print_tx
                            .send(format!("error deserializing message: {}", e))
                            .await;
                        continue;
                    }
                };
                ingest_peer_msg(message_tx.clone(), print_tx.clone(), message).await;
            }
            Err(e) => {
                let _ = print_tx
                    .send(format!(
                        "lost connection to peer {}!\nerror: {}",
                        who.name, e
                    ))
                    .await;
                peers.write().await.remove(&who.name);
                break;
            }
        }
    }
}

/// takes in two streams. every message received on stream_1 is forwarded to stream_2,
/// and every message received on stream_2 is forwarded to stream_1.
async fn active_router(stream_1: Sock, stream_2: Sock, print_tx: PrintSender) {
    let (write_stream_1, read_stream_1) = stream_1.split();
    let (write_stream_2, read_stream_2) = stream_2.split();

    tokio::select! {
        _ = forwarder(read_stream_1, write_stream_2, print_tx.clone()) => {},
        _ = forwarder(read_stream_2, write_stream_1, print_tx.clone()) => {},
    }
}

async fn forwarder(
    mut read: SplitStream<Sock>,
    mut write: SplitSink<Sock, tungstenite::Message>,
    print_tx: PrintSender,
) {
    while let Some(msg) = read.next().await {
        let _ = print_tx.send(format!("routing a message...")).await;
        match msg {
            Ok(msg) => match write.send(msg).await {
                Ok(_) => {}
                Err(e) => {
                    let _ = print_tx.send(format!("we failed as a router: {}", e)).await;
                    break;
                }
            },
            Err(e) => {
                let _ = print_tx.send(format!("we failed as a router: {}", e)).await;
                break;
            }
        }
    }
}

/// take in a decrypted message received over network and send it to kernel
async fn ingest_peer_msg(message_tx: MessageSender, print_tx: PrintSender, msg: Message) {
    // if payload is just a string, print it as a "message"
    // otherwise forward to kernel for processing
    match (&msg.payload.json, &msg.payload.bytes) {
        (Some(serde_json::Value::String(s)), None) => {
            let _ = print_tx
                .send(format!(
                    "\x1b[3;32m {}: {:?} \x1b[0m",
                    msg.wire.source_ship, s
                ))
                .await;
        }
        _ => {
            let _ = message_tx.send(vec![msg]).await;
        }
    }
}
