use futures::stream::{SplitSink, SplitStream};
use tokio::net::{TcpStream, TcpListener};
use tokio_tungstenite::tungstenite::Error;
use tokio_tungstenite::{connect_async, tungstenite::Message, accept_async, tungstenite::Result};
use url::Url;
use futures::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tokio_tungstenite::{WebSocketStream, MaybeTlsStream};

use crate::types::*;

use ethers::prelude::*;

#[derive(Debug)]
pub struct Peer {
    pub name: String,
    pub ws_url: String,
    pub ws_port: u16,
    pub ws_write_stream: SplitSink<Sock, Message>,
}

pub type Peers = Arc<Mutex<HashMap<H256, Peer>>>;
pub type Sock = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// websockets driver.
///
/// statelessly manages websocket connections to peer nodes.
pub async fn websockets(our: &Identity,
                        pki: &OnchainPKI,
                        card_rx: CardReceiver,
                        card_tx: CardSender,
                        print_tx: PrintSender) {

    let tcp_listener = TcpListener::bind(format!("0.0.0.0:{}", our.ws_port)).await
                       .expect(format!("error: can't listen on port {}", our.ws_port).as_str());

    // initialize peer-connection-map as empty -- can pre-populate as optimization?
    let peers: Peers = Arc::new(Mutex::new(HashMap::new()));

    let _ = print_tx.send(format!("now listening on port {}", our.ws_port)).await;

    // listen on our port for new connections, and
    // listen on our receiver for messages to send to peers
    tokio::join!(
        ws_listener(tcp_listener, peers.clone(), card_tx.clone(), print_tx.clone()),
        ws_sender(our, pki, peers.clone(), card_rx, card_tx.clone(), print_tx.clone()),
    );

}

async fn ws_listener(tcp: TcpListener, peers: Peers, card_tx: CardSender, print_tx: PrintSender) {
    while let Ok((stream, _socket_addr)) = tcp.accept().await {
        // XX surface the result of this conn
        tokio::spawn(handle_connection(stream, peers.clone(), card_tx.clone(), print_tx.clone()));
    }
}

async fn handle_connection(stream: TcpStream, peers: Peers, card_tx: CardSender, print_tx: PrintSender) -> Result<(), Error> {
    let ws_stream = accept_async(MaybeTlsStream::Plain(stream)).await?;
    // place the connection, split in two, inside peer mapping
    let (write_stream, mut read_stream) = ws_stream.split();

    // take first message on stream and use it to identify peer
    let handshake_msg = read_stream.next().await.ok_or(Error::ConnectionClosed)??;
    // XX verify with a signature or whatever
    let id: Identity = serde_json::from_str(&handshake_msg.to_string()).unwrap();

    let _ = print_tx.send(format!("shook hands with new peer: {}", id.name)).await;

    // add peer to peer mapping
    let mut edit = peers.lock().await;
    edit.insert(id.address, Peer {
                                name: id.name,
                                ws_url: id.ws_url,
                                ws_port: id.ws_port,
                                ws_write_stream: write_stream,
                            });
    drop(edit);

    tokio::spawn(active_reader(read_stream, card_tx.clone(), print_tx.clone()));

    Ok(())
}

async fn active_reader(mut read_stream: SplitStream<Sock>, card_tx: CardSender, print_tx: PrintSender) {
    let _ = print_tx.send(format!("actively listening to socket")).await;
    while let Some(msg) = read_stream.next().await {
        match msg {
            Ok(msg) => {
                ingest_peer_msg(card_tx.clone(), print_tx.clone(), msg).await;
            }
            Err(_) => {
                // we lost a peer connection. send card to kernel to try and reconnect?
                let _ = print_tx.send(format!("lost peer conn")).await;
                break;
            }
        }
    }
}

pub async fn ws_sender(our: &Identity,
                       pki: &OnchainPKI,
                       peers: Peers,
                       mut card_rx: CardReceiver,
                       card_tx: CardSender,
                       print_tx: PrintSender) {
    while let Some(card) = card_rx.recv().await {
        let mut to = peers.lock().await;

        match to.remove(&card.target) {
            Some(mut peer) => {
                // use existing write stream to send message
                match peer.ws_write_stream.send(Message::text(serde_json::to_string(&card).unwrap())).await {
                    Ok(_) => {
                        let _ = print_tx.send(format!("sent card to {}", peer.name)).await;
                        to.insert(card.target, peer);
                    }
                    Err(e) => {
                        let _ = print_tx.send(format!("error sending card: {}", e)).await;
                        to.insert(card.target, peer);
                    }
                }
            },
            None => {
                // try to open a new connection
                let peer = pki.get(&card.target).unwrap();

                match connect_async(Url::parse(&format!("ws://{}:{}/ws", peer.ws_url, peer.ws_port)).unwrap()).await {
                    Ok((mut socket, _socket_addr)) => {
                        let _ = print_tx.send(format!("shaking hands with {}", peer.name)).await;
                        // send handshake message with our Identity
                        let _ = socket.send(Message::text(serde_json::to_string(our).unwrap())).await;
                        // then send actual message
                        let _ = socket.send(Message::text(serde_json::to_string(&card).unwrap())).await;
                        // then store connection in our peer map
                        // Convert MaybeTlsStream to TcpStream
                        let (write_stream, read_stream) = socket.split();
                        to.insert(card.target, Peer {
                                                    name: peer.name.clone(),
                                                    ws_url: peer.ws_url.clone(),
                                                    ws_port: peer.ws_port,
                                                    ws_write_stream: write_stream,
                                                 });
                        drop(to);
                        // and start reading from the stream
                        // XX surface the result of this conn
                        tokio::spawn(active_reader(read_stream, card_tx.clone(), print_tx.clone()));
                    },
                    Err(_) => continue,
                }
            }
        }
    }
}

async fn ingest_peer_msg(card_tx: CardSender, print_tx: PrintSender, msg: Message) {
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
    let card: Card = match serde_json::from_str(&message) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error while parsing message: {}", e);
            return;
        }
    };
    let duration = start.elapsed();
    let _ = print_tx.send(format!("Time taken to deserialize: {:?}", duration)).await;
    let _ = card_tx.send(card).await;
}
