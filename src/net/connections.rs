use crate::net::*;

use aes_gcm::aead::Aead;
use aes_gcm::KeyInit;
use aes_gcm_siv::{Aes256GcmSiv, Nonce};
use futures::{SinkExt, StreamExt};
use ring::signature::Ed25519KeyPair;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::{self};

pub async fn build_connection(
    our: Identity,
    keypair: Arc<Ed25519KeyPair>,
    target: Option<Identity>,
    initial_message: Option<(WrappedMessage, oneshot::Sender<Result<(), NetworkError>>)>,
    pki: OnchainPKI,
    peers: Peers,
    mut websocket: WebSocket,
    kernel_message_tx: MessageSender,
) {
    println!("build_connection");
    let (cipher, nonce, their_id) = match target {
        Some(target) => {
            // we have target, we are initiating
            let (ephemeral_secret, our_handshake) =
                make_secret_and_handshake(&our, keypair, target.name.clone(), None, false);
            let _ = websocket
                .send(tungstenite::Message::Text(
                    serde_json::to_string(&our_handshake).unwrap(),
                ))
                .await;
            let their_handshake = get_handshake(&mut websocket).await.unwrap();
            let (their_ephemeral_pk, nonce) =
                match validate_handshake(&their_handshake, &target, our_handshake.nonce.clone()) {
                    Ok(v) => v,
                    Err(_) => return,
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
                Err(_) => return,
            };
            let (ephemeral_secret, our_handshake) =
                make_secret_and_handshake(&our, keypair, their_id.name.clone(), None, false);
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
    let (sender_tx, sender_rx) =
        mpsc::unbounded_channel::<(WrappedMessage, oneshot::Sender<Result<(), NetworkError>>)>();
    let (socket_tx, socket_rx) = mpsc::unbounded_channel::<(
        NetworkMessage,
        Option<oneshot::Sender<Result<(), NetworkError>>>,
    )>();
    let (handler_tx, handler_rx) = mpsc::unbounded_channel::<Vec<u8>>();

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
        peers.clone(),
        socket_tx,
        socket_rx,
        websocket,
    ));
    // connection is now ready to write to
    tokio::spawn(active_peer(
        their_id.name.clone(),
        peer_handler,
        connection_handler,
        peers.clone(),
    ));
    println!("stuck here");
    peers.write().await.insert(their_id.name.clone(), peer);
    println!("not here");
    if let Some(to_send) = initial_message {
        println!("sending initial");
        let _ = sender_tx.send(to_send);
    }
}

async fn active_peer(
    who: String,
    peer_handler: JoinHandle<()>,
    connection_handler: JoinHandle<()>,
    peers: Peers,
) {
    println!("active_peer");
    let _err = tokio::select! {
        _ = peer_handler => {},
        _ = connection_handler => {},
    };
    println!("lost active_peer, deleting peer!");
    peers.write().await.remove(&who);
}

/// send and receive messages on an existing websocket
/// if this breaks it's a timeout
async fn maintain_connection(
    peers: Peers,
    self_tx: mpsc::UnboundedSender<(
        NetworkMessage,
        Option<oneshot::Sender<Result<(), NetworkError>>>,
    )>,
    mut message_rx: mpsc::UnboundedReceiver<(
        NetworkMessage,
        Option<oneshot::Sender<Result<(), NetworkError>>>,
    )>,
    websocket: WebSocket,
) {
    println!("maintain_connection");
    let (mut write_stream, mut read_stream) = websocket.split();
    let (ack_tx, mut ack_rx) = mpsc::unbounded_channel::<u64>();

    let message_sender = tokio::spawn(async move {
        while let Some((message, result_tx)) = message_rx.recv().await {
            write_stream
                .send(tungstenite::Message::Binary(
                    serde_json::to_vec(&message).unwrap(),
                ))
                .await
                .expect("Failed to send a message");
            match message {
                NetworkMessage::Ack(_) => continue,
                NetworkMessage::Msg { .. } => {
                    println!("message sent");
                    // await for the ack with timeout
                    match timeout(TIMEOUT, ack_rx.recv()).await {
                        Ok(Some(_ack)) => {
                            // message delivered
                            println!("message delivered");
                            result_tx.unwrap().send(Ok(())).unwrap();
                            continue;
                        }
                        _ => {
                            println!("Timeout!!");
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
                    match msg {
                        NetworkMessage::Ack(id) => {
                            let _ = ack_tx.send(id);
                            continue;
                        }
                        NetworkMessage::Msg { from, contents, .. } => {
                            if let Some(peer) = peers.write().await.get(&from) {
                                if let Ok(()) = peer.handler.send(contents) {
                                    // TODO id
                                    let _ = self_tx.send((NetworkMessage::Ack(0), None));
                                    continue;
                                }
                            }
                        }
                    }
                }
            }
            // bad bad
            break;
        }
    });

    // if either task completes, the socket is lost
    tokio::select! {
        _ = message_sender => {},
        _ = message_receiver => {},
    };
    println!("lost maintain_connection!");
}

/// 1. take in messages from a specific peer, decrypt them, and send to kernel
/// 2. take in messages targeted at specific peer, encrypt them, and send to proper connection
async fn peer_handler(
    our: Identity,
    who: String,
    cipher: Aes256GcmSiv,
    nonce: Arc<Nonce>,
    forwarder: mpsc::UnboundedReceiver<(WrappedMessage, oneshot::Sender<Result<(), NetworkError>>)>,
    mut receiver: mpsc::UnboundedReceiver<Vec<u8>>,
    socket_tx: mpsc::UnboundedSender<(
        NetworkMessage,
        Option<oneshot::Sender<Result<(), NetworkError>>>,
    )>,
    kernel_message_tx: MessageSender,
) {
    println!("peer_handler");
    let f_nonce = nonce.clone();
    let f_cipher = cipher.clone();

    let forwarder = tokio::spawn(async move {
        let socket_tx = socket_tx;
        let mut forwarder = forwarder;
        while let Some((message, result_tx)) = forwarder.recv().await {
            if let Ok(bytes) = serde_json::to_vec(&message) {
                if let Ok(encrypted) = f_cipher.encrypt(&f_nonce, bytes.as_ref()) {
                    let _ = socket_tx.send((
                        NetworkMessage::Msg {
                            from: our.name.clone(),
                            to: who.clone(),
                            contents: encrypted,
                        },
                        Some(result_tx),
                    ));
                }
            }
        }
    });

    tokio::select! {
        _ = forwarder => {},
        _ = async {
            while let Some(encrypted_bytes) = receiver.recv().await {
                if let Ok(decrypted) = cipher.decrypt(&nonce, encrypted_bytes.as_ref()) {
                    if let Ok(message) = serde_json::from_slice::<WrappedMessage>(&decrypted) {
                        let _ = kernel_message_tx.send(message).await;
                        continue;
                    }
                }
                break;
            }
        } => {}
    };
    println!("lost peer_handler!");
}
