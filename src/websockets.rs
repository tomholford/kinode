use tokio::net::{TcpStream, TcpListener};
use tokio_tungstenite::tungstenite::Error;
use tokio_tungstenite::{connect_async, accept_async, tungstenite};
// use tokio_tungstenite::{connect_async, tungstenite::Message, accept_async, tungstenite::Result};
use url::Url;
use futures::prelude::*;
use std::time::Instant;

use crate::types::*;

/*
 *  websockets driver
 */
pub async fn ws_listener(message_tx: MessageSender, print_tx: PrintSender, tcp: TcpListener) {
    while let Ok((stream, _)) = tcp.accept().await {
        let socket_addr = stream.peer_addr().expect("connected streams should have a peer address");
        let _ = print_tx.send(format!("Peer address: {}", socket_addr)).await;

        tokio::spawn(handle_connection(stream, message_tx.clone(), print_tx.clone()));
    }
}

pub async fn ws_sender(peers: Peers, print_tx: PrintSender, mut rx: MessageReceiver) {
    while let Some(message) = rx.recv().await {
        let last_message_index = message.len() - 1;
        let mut to = peers.write().await;
        match to.remove(&message[last_message_index].wire.target_ship) {
            Some(peer) => {
                match handle_send(&message, &peer.url, &peer.port, peer.connection).await {
                    Ok(new_conn) => {
                        let _ = print_tx.send("message sent!".to_string()).await;
                        to.insert(message[last_message_index].wire.target_ship.clone(), Peer {connection: Some(new_conn), ..peer});
                    }
                    Err(e) => {
                        let _ = print_tx.send(format!("error sending message: {}", e)).await;
                        to.insert(message[last_message_index].wire.target_ship.clone(), Peer {connection: None, ..peer});
                    }
                }
            },
            None => {
                let _ = print_tx.send("error sending message, no known peer".into()).await;
            }
        }
    }
}

/// send a message to a peer over websocket.
/// if we don't have an active connection to peer, try to make one
async fn handle_send(message: &MessageStack, peer_url: &str, peer_port: &u16, peer_conn: Option<Sock>)
    -> tungstenite::Result<Sock, Error> {
    match peer_conn {
        Some(mut socket) => {
            match socket.send(tungstenite::Message::text(serde_json::to_string(message).unwrap())).await {
                Ok(_) => Ok(socket),
                Err(e) => Err(e)
            }
        },
        None => {
            match connect_async(Url::parse(&format!("ws://{}:{}/ws", peer_url, peer_port)).unwrap()).await {
                Ok((mut socket, _)) => {
                    socket.send(tungstenite::Message::text(serde_json::to_string(message).unwrap())).await.unwrap();
                    Ok(socket)
                },
                Err(e) => Err(e)
            }
        }
    }
}

async fn handle_connection(stream: TcpStream, message_tx: MessageSender, print_tx: PrintSender) {
    let mut ws_stream = accept_async(stream).await.expect("Failed to accept");

    while let Some(msg) = ws_stream.next().await {
        match msg {
            Ok(msg) => {
                ingest_peer_msg(message_tx.clone(), print_tx.clone(), msg).await;
            }
            Err(e) => {
                println!("error while reading from socket: {}", e);
                break;
            }
        }
    }
}

async fn ingest_peer_msg(message_tx: MessageSender, print_tx: PrintSender, msg: tungstenite::Message) {
    let message = match msg.into_text() {
        Ok(v) => v,
        Err(_) => return,
    };

    if message == "ack!" {
        let _ = print_tx.send(message).await;
        return
    }
    // deserialize
    let start = Instant::now();
    let messages: MessageStack = match serde_json::from_str(&message) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error while parsing message: {}", e);
            return;
        }
    };
    let duration = start.elapsed();
    let _ = print_tx.send(format!("Time taken to deserialize: {:?}", duration)).await;
    let _ = message_tx.send(messages).await;
}
