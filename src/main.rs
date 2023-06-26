use std::{collections::HashMap, sync::Arc};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, RwLock};
use tokio::time::sleep;
use std::{env, time::Duration};

use crate::types::*;

mod types;
mod terminal;
mod websockets;

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
        term = terminal::terminal(&our_name, card_sender.clone(), print_receiver) => match term {
            Ok(_) => "graceful shutdown".to_string(),
            Err(e) => format!("exiting with error: {:?}", e),
        },
        _ = send_cards(&our_name, peers.clone(), print_sender.clone(), card_receiver) => {
            "card sender died".to_string()
        },
        _ = websockets::ws_listener(print_sender.clone(), listener) => {
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
                    match websockets::ws_publisher(&card, peer).await {
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
