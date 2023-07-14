use futures::prelude::*;
use futures::stream::{SplitSink, SplitStream};
use std::collections::HashMap;
use std::sync::Arc;
// use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::{accept_async, connect_async, tungstenite::Result};  // tungstenite::Message overloads our Message, so refer to it as tungstenite::Message in code
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use url::Url;

use crate::types::*;
use ethers::prelude::*;

#[derive(Debug)]
pub struct Peer {
    pub address: H256,
    pub ws_url: String,
    pub ws_port: u16,
    pub ws_write_stream: SplitSink<Sock, tungstenite::Message>,
}

pub type Peers = Arc<RwLock<HashMap<String, Peer>>>;
pub type Sock = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// websockets driver.
///
/// statelessly manages websocket connections to peer nodes.
pub async fn websockets(
    our: &Identity,
    pki: &OnchainPKI,
    message_rx: MessageReceiver,
    message_tx: MessageSender,
    print_tx: PrintSender,
) {
    let tcp_listener = TcpListener::bind(format!("0.0.0.0:{}", our.ws_port))
        .await
        .expect(format!("error: can't listen on port {}", our.ws_port).as_str());

    // initialize peer-connection-map as empty -- can pre-populate as optimization?
    let peers: Peers = Arc::new(RwLock::new(HashMap::new()));

    let _ = print_tx
        .send(format!("now listening on port {}", our.ws_port))
        .await;

    // listen on our port for new connections, and
    // listen on our receiver for messages to send to peers
    tokio::join!(
        ws_listener(
            tcp_listener,
            peers.clone(),
            message_tx.clone(),
            print_tx.clone(),
        ),
        ws_sender(
            our,
            pki,
            peers.clone(),
            message_rx,
            message_tx.clone(),
            print_tx.clone(),
        ),
    );
}

async fn ws_listener(tcp: TcpListener, peers: Peers, message_tx: MessageSender, print_tx: PrintSender) {
    while let Ok((stream, _socket_addr)) = tcp.accept().await {
        let closed =
            handle_connection(stream, peers.clone(), message_tx.clone(), print_tx.clone()).await;
        match closed {
            Ok(()) => continue,
            Err(e) => {
                let _ = print_tx
                    .send(format!("peer connection closed: {}", e))
                    .await;
            }
        }
    }
}

async fn handle_connection(
    stream: TcpStream,
    peers: Peers,
    message_tx: MessageSender,
    print_tx: PrintSender,
) -> Result<(), tungstenite::Error> {
    let ws_stream = accept_async(MaybeTlsStream::Plain(stream)).await?;
    // place the connection, split in two, inside peer mapping
    let (write_stream, mut read_stream) = ws_stream.split();

    // take first message on stream and use it to identify peer
    let handshake_msg = read_stream.next().await.ok_or(tungstenite::Error::ConnectionClosed)??;
    // XX verify with a signature or whatever
    let id: Identity = serde_json::from_str(&handshake_msg.to_string()).unwrap();

    let _ = print_tx
        .send(format!("shook hands with new peer: {:#?}", id))
        .await;

    // add them to peer mapping
    drop(peers.write().await.insert(
        id.name.clone(),
        Peer {
            address: id.address,
            ws_url: id.ws_url,
            ws_port: id.ws_port,
            ws_write_stream: write_stream,
        },
    ));

    let _closed = active_reader(read_stream, message_tx.clone(), print_tx.clone()).await;

    // remove peer, connection was closed!
    peers.write().await.remove(&id.name);

    Ok(())
}

async fn active_reader(
    mut read_stream: SplitStream<Sock>,
    message_tx: MessageSender,
    print_tx: PrintSender,
) {
    let _ = print_tx.send(format!("actively listening to socket")).await;
    while let Some(msg) = read_stream.next().await {
        match msg {
            Ok(msg) => {
                ingest_peer_msg(message_tx.clone(), print_tx.clone(), msg).await;
            }
            Err(_) => {
                // we lost a peer connection. send message to kernel to try and reconnect?
                let _ = print_tx.send(format!("lost peer conn")).await;
                break;
            }
        }
    }
}

pub async fn ws_sender(
    our: &Identity,
    pki: &OnchainPKI,
    peers: Peers,
    mut card_rx: MessageReceiver,
    card_tx: MessageSender,  // to change: card -> message
    print_tx: PrintSender,
) {
    while let Some(message) = card_rx.recv().await {
        let mut edit = peers.write().await;
        let target = &message.target.server;
        match edit.remove(target) {
            Some(mut peer) => {
                // let _ = print_tx.send(format!("sending card to existing peer {}", &card.target)).await;
                // use existing write stream to send message
                match peer
                    .ws_write_stream
                    .send(tungstenite::Message::text(serde_json::to_string(&message).unwrap()))
                    .await
                {
                    Ok(_) => {
                        let _ = print_tx
                            .send(format!("sent card to {}", target))
                            .await;
                        edit.insert(message.target.server, peer);
                    }
                    Err(e) => {
                        let _ = print_tx.send(format!("error sending card: {}", e)).await;
                        edit.remove(target);
                    }
                }
            }
            None => {
                // try to open a new connection
                let _ = print_tx
                    .send(format!("trying to open new conn to {}", target))
                    .await;
                let id = pki.get(target).unwrap();

                match connect_async(
                    Url::parse(&format!("ws://{}:{}/ws", id.ws_url, id.ws_port)).unwrap(),
                )
                .await
                {
                    Ok((mut socket, _socket_addr)) => {
                        let _ = print_tx
                            .send(format!("shaking hands with {}", id.name))
                            .await;
                        // send handshake message with our Identity
                        let _ = socket
                            .send(tungstenite::Message::text(serde_json::to_string(our).unwrap()))
                            .await;
                        // then send actual message
                        let _ = socket
                            .send(tungstenite::Message::text(serde_json::to_string(&message).unwrap()))
                            .await;
                        // then store connection in our peer map
                        // Convert MaybeTlsStream to TcpStream
                        let (write_stream, read_stream) = socket.split();
                        edit.insert(
                            message.target.server,
                            Peer {
                                address: id.address.clone(),
                                ws_url: id.ws_url.clone(),
                                ws_port: id.ws_port,
                                ws_write_stream: write_stream,
                            },
                        );
                        drop(edit);

                        // and start reading from the stream
                        tokio::spawn(make_reader(
                            id.name.clone(),
                            peers.clone(),
                            read_stream,
                            card_tx.clone(),
                            print_tx.clone(),
                        ));
                    }
                    Err(_) => continue,
                }
            }
        }
    }
}

async fn make_reader(
    who: String,
    peers: Peers,
    read_stream: SplitStream<Sock>,
    message_tx: MessageSender,
    print_tx: PrintSender,
) {
    let _closed = active_reader(read_stream, message_tx.clone(), print_tx.clone()).await;

    // remove peer, connection was closed!
    peers.write().await.remove(&who);
}

async fn ingest_peer_msg(message_tx: MessageSender, print_tx: PrintSender, msg: tungstenite::Message) {
    let message = match msg.into_text() {
        Ok(v) => v,
        Err(_) => {
            let _ = print_tx
                .send("got weird message over websocket".to_string())
                .await;
            return;
        }
    };

    // deserialize
    // let start = Instant::now();
    let message: Message = match serde_json::from_str(&message) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error while parsing message: {}", e);
            return;
        }
    };
    // let duration = start.elapsed();
    // let _ = print_tx
    //     .send(format!("Time taken to deserialize: {:?}", duration))
    //     .await;

    // if payload is just a string, print it as a "message"
    // otherwise forward to kernel for processing
    match (&message.payload.json, &message.payload.bytes) {
        (Some(serde_json::Value::String(s)), None) => {
            let _ = print_tx
                .send(format!(
                    "\x1b[3;32m {}: {:?} \x1b[0m",
                    message.source.server,
                    s
                ))
                .await;
        },
        _ => {
            let _ = message_tx.send(message).await;
        },
    }
}
