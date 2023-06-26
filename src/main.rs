use std::{collections::HashMap, sync::Arc};
use log::*;
use serde::{Serialize, Deserialize};
use tokio::net::{TcpStream, TcpListener};
use tokio::sync::{mpsc, RwLock};
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Error;
use tokio_tungstenite::{WebSocketStream, MaybeTlsStream, connect_async, tungstenite::Message, accept_async, tungstenite::Result};
use url::Url;
use std::{io::Write, env, time::Duration};
use rustyline_async::{Readline, ReadlineError};
use futures::prelude::*;
use std::time::Instant;

type Peers = Arc<RwLock<HashMap<String, Peer>>>;

type Sock = WebSocketStream<MaybeTlsStream<TcpStream>>;

type CardSender = tokio::sync::mpsc::Sender<Card>;
type CardReceiver = tokio::sync::mpsc::Receiver<Card>;

type PrintSender = tokio::sync::mpsc::Sender<String>;
type PrintReceiver = tokio::sync::mpsc::Receiver<String>;

type BlockchainPKI = HashMap<String, Identity>;

#[derive(Debug, Serialize, Deserialize)]
pub struct Identity {
    pub name: String,
    pub url: String,
    pub port: u16,
}

#[derive(Debug)]
pub struct Peer {
    pub name: String,
    pub url: String,
    pub port: u16,
    pub connection: Option<Sock>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Card {
    source: String,
    target: String,
    payload: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct ID {
    node: String,
    app_name: String,
    app_distributor: String,
    app_version: String,
}

enum Command {
    Card(Card),
    Quit,
    Invalid,
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    let our_name: String = args[1].clone();

    let (card_sender, card_receiver): (CardSender, CardReceiver) = mpsc::channel(32);
    let (print_sender, print_receiver): (PrintSender, PrintReceiver) = mpsc::channel(32);

    let blockchain = std::fs::File::open("blockchain.json")
        .expect("couldn't read from the chain lolz");
    let json: serde_json::Value = serde_json::from_reader(blockchain)
        .expect("file should be proper JSON");
    let pki = serde_json::from_value::<BlockchainPKI>(json).expect("should be a list of peers");
    let mut peermap = HashMap::new();
    for (name, id) in pki {
        peermap.insert(
            name.clone(),
            Peer {
                name: id.name,
                url: id.url,
                port: id.port,
                connection: None,
            },
        );
    }

    let our_port: u16 = peermap.get(&our_name).unwrap().port;

    let peers: Peers = Arc::new(RwLock::new(peermap));

    let listener = TcpListener::bind(format!("127.0.0.1:{}", our_port)).await.expect("Can't listen");

    let quit: String = tokio::select! {
        _ = print_loop(peers.clone(), print_sender.clone()) => "print loop died".to_string(),
        term = terminal(&our_name, card_sender.clone(), print_receiver) => match term {
            Ok(_) => "graceful shutdown".to_string(),
            Err(e) => format!("exiting with error: {:?}", e),
        },
        _ = send_cards(&our_name, peers.clone(), print_sender.clone(), card_receiver) => {
            "card sender died".to_string()
        },
        _ = ws_listener(print_sender.clone(), listener) => {
            "websocket server died".to_string()
        }
    };

    println!("{}", quit);
}

async fn print_loop(peers: Peers, print_tx: PrintSender) {
    loop {
        sleep(Duration::from_secs(5)).await;
        let peeps = peers.read().await;
        let _ = print_tx.send(format!("known peers: {:#?}", peeps.keys())).await;
    }
}

async fn send_cards(our_name: &str, peers: Peers, print_tx: PrintSender, mut rx: CardReceiver) {
    while let Some(card) = rx.recv().await {
        if card.target == our_name {
            //  self-sent card, give to "mars"
            //  let _ = writeln!(stdout, "got card: {}", serde_json::to_string(&card).unwrap());
            //  if we get a card for ourselves handle it somewhere.. not here
        } else {
            let mut to = peers.write().await;
            match to.remove(&card.target) {
                Some(peer) => {
                    match publish_handler(&card, peer).await {
                        Ok(new_peer) => {
                            to.insert(card.target.clone(),new_peer);
                        }
                        Err(e) => {
                            print_tx.send(format!("error sending card: {}", e)).await.unwrap();
                        }
                    }
                },
                None => {
                    print_tx.send("error sending card, no known peer".into()).await.unwrap();
                }
            }
        }
    }
}

fn parse_command(our_name: &str, line: &str) -> Command {
    let mut parts = line.split_whitespace();
    match parts.next() {
        Some("!card") => {
            Command::Card(Card {
                source: our_name.to_string(),
                target: parts.next().unwrap().to_string(),
                payload: serde_json::json!({"message": parts.collect::<Vec<&str>>().join(" ")}),
            })
        }
        Some("!quit") => Command::Quit,
        Some("!exit") => Command::Quit,
        _ => Command::Invalid,
    }
}

/*
 *  terminal driver
 */
async fn terminal(our_name: &str, card_tx: CardSender, mut print_rx: PrintReceiver)
    -> core::result::Result<(), ReadlineError> {

    let (mut rl, mut stdout) = Readline::new("> ".into()).unwrap();

    loop {
        tokio::select! {
            prints = print_rx.recv() => match prints {
                Some(print) => { writeln!(stdout, "{}", print)?; },
                None => { break; }
            },
            cmd = rl.readline() => match cmd {
                Ok(line) => {
                    rl.add_history_entry(line.clone());
                    match parse_command(our_name, &line) {
                        Command::Card(card) => {
                            card_tx.send(card).await.unwrap();
                            writeln!(stdout, "{}", line)?;
                        },
                        Command::Quit => {
                            break;
                        },
                        Command::Invalid => {
                            writeln!(stdout, "invalid command: {}", line)?;
                        }
                    }
                }
                Err(ReadlineError::Eof) => {
                    writeln!(stdout, "<EOF>")?;
                    break;
                }
                Err(ReadlineError::Interrupted) => { break; }
                Err(e) => {
                    writeln!(stdout, "Error: {e:?}")?;
                    break;
                }
            }
        }
    }
    rl.flush()?;
    Ok(())
}

/*
 *  websockets driver
 */
async fn ws_listener(print_tx: PrintSender, tcp: TcpListener) {
    while let Ok((stream, _)) = tcp.accept().await {
        let socket_addr = stream.peer_addr().expect("connected streams should have a peer address");
        info!("Peer address: {}", socket_addr);

        tokio::spawn(handle_connection(stream, print_tx.clone()));
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

async fn publish_handler(card: &Card, peer: Peer) -> Result<Peer, Error> {
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
