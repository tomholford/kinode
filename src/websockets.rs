use log::*;
use tokio::net::{TcpStream, TcpListener};
use tokio_tungstenite::tungstenite::Error;
use tokio_tungstenite::{connect_async, tungstenite::Message, accept_async, tungstenite::Result};
use url::Url;
use futures::prelude::*;
use std::time::Instant;

use crate::types::*;

/*
 *  websockets driver
 */
pub async fn ws_listener(print_tx: PrintSender, tcp: TcpListener) {
    while let Ok((stream, _)) = tcp.accept().await {
        let socket_addr = stream.peer_addr().expect("connected streams should have a peer address");
        info!("Peer address: {}", socket_addr);

        tokio::spawn(handle_connection(stream, print_tx.clone()));
    }
}

pub async fn ws_publisher(card: &Card, peer: Peer) -> Result<Peer, Error> {
    match peer.connection {
        Some(mut socket) => {
            match socket.send(Message::text(serde_json::to_string(card).unwrap())).await {
                Ok(_) => Ok(Peer {connection: Some(socket), ..peer}),
                Err(e) => Err(e)
            }
        },
        None => {
            match connect_async(Url::parse(&format!("ws://{}:{}/ws", &peer.url, &peer.port)).unwrap()).await {
                Ok((mut socket, _)) => {
                    socket.send(Message::text(serde_json::to_string(card).unwrap())).await.unwrap();
                    Ok(Peer {connection: Some(socket), ..peer})
                },
                Err(e) => Err(e)
            }
        }
    }
}

async fn handle_connection(stream: TcpStream, print_tx: PrintSender) {
    let mut ws_stream = accept_async(stream).await.expect("Failed to accept");

    while let Some(msg) = ws_stream.next().await {
        match msg {
            Ok(msg) => {
                ingest_peer_msg(print_tx.clone(), msg).await;
            }
            Err(e) => {
                println!("error while reading from socket: {}", e);
                break;
            }
        }
    }
}

async fn ingest_peer_msg(print_tx: PrintSender, msg: Message) {
    let message = match msg.into_text() {
        Ok(v) => v,
        Err(_) => return,
    };

    if message == "ack" {
        let _ = print_tx.send(format!("got ack")).await;
    }
    // deserialize
    let start = Instant::now();
    let card: Card = match serde_json::from_str(&message) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error while parsing message: {}", e);
            return;
        }
    };
    let duration = start.elapsed();
    println!("Time taken to deserialize: {:?}", duration);
    // serialize
    let start = Instant::now();
    let serialized = serde_json::to_string(&card).unwrap();
    let duration = start.elapsed();
    println!("Time taken to serialize: {:?}", duration);
    let _ = print_tx.send(format!("got card: {}", serialized)).await;
}
