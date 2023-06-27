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

async fn handle_card(card: Card, peers: Peers, print_tx: PrintSender) {
    print_tx.send(format!("got card: {:?}", card)).await.unwrap();

    match serde_json::from_value::<KernelCommand>(card.payload) {
        Ok(cmd) => {
            match cmd {
                KernelCommand::ShowPeers => {
                    let peers = peers.read().await;
                    print_tx.send(format!("peers: {:?}", peers.keys())).await.unwrap();
                }
            }
        }
        Err(e) => {
            print_tx.send(format!("error parsing card: {}", e)).await.unwrap();
        }
    }
}
