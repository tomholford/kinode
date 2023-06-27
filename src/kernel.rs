use serde::{Serialize, Deserialize};

use crate::types::*;
use crate::websockets;

#[derive(Debug, Serialize, Deserialize)]
enum KernelCommand {
    //  TODO add a lot more
    ShowPeers,
}

pub async fn kernel(our_name: &str, peers: Peers, print_tx: PrintSender, mut rx: CardReceiver) {
    while let Some(card) = rx.recv().await {
        if card.target == our_name {
            handle_card(card, peers.clone(), print_tx.clone()).await;
        } else {
            let mut to = peers.write().await;
            match to.remove(&card.target) {
                Some(peer) => {
                    match websockets::ws_sender(&card, &peer.url, &peer.port, peer.connection).await {
                        Ok(new_conn) => {
                            let _ = print_tx.send("card sent!".to_string()).await;
                            to.insert(card.target, Peer {connection: Some(new_conn), ..peer});
                        }
                        Err(e) => {
                            let _ = print_tx.send(format!("error sending card: {}", e)).await;
                            to.insert(card.target, Peer {connection: None, ..peer});
                        }
                    }
                },
                None => {
                    let _ = print_tx.send("error sending card, no known peer".into()).await;
                }
            }
        }
    }
}

async fn handle_card(card: Card, peers: Peers, print_tx: PrintSender) {
    print_tx.send(format!("got card: {:#?}", card)).await.unwrap();

    match serde_json::from_value::<KernelCommand>(card.payload) {
        Ok(cmd) => {
            match cmd {
                KernelCommand::ShowPeers => {
                    let peers = peers.read().await;
                    print_tx.send(format!("peers: {:?}", peers.keys())).await.unwrap();
                }
            }
        }
        Err(_) => {
            print_tx.send("no destination for card".to_string()).await.unwrap();
        }
    }
}
