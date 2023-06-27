use std::{collections::HashMap, sync::Arc};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, RwLock};
use std::env;

use crate::types::*;

mod types;
mod terminal;
mod websockets;
mod kernel;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    let our_name: String = args[1].clone();

    // kernel receives system cards via this channel, all other modules send cards
    let (card_sender, card_receiver): (CardSender, CardReceiver) = mpsc::channel(32);
    // terminal receives prints via this channel, all other modules send prints
    let (print_sender, print_receiver): (PrintSender, PrintReceiver) = mpsc::channel(32);

    // this will be replaced with actual chain reading
    let blockchain = std::fs::File::open("blockchain.json")
        .expect("couldn't read from the chain lolz");
    let json: serde_json::Value = serde_json::from_reader(blockchain)
        .expect("blockchain.json should be proper JSON");
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

    let our_port: u16 = peermap.get(&our_name).expect("must use a name in blockchain.json").port;

    let peers: Peers = Arc::new(RwLock::new(peermap));

    let tcp_listener = TcpListener::bind(format!("0.0.0.0:{}", our_port)).await.expect("Can't listen");

    /*  we are currently running 2 I/O modules: terminal, websocket
     *  the kernel module will handle our userspace processes and receives
     *  all "cards", the basic message format for uqbar.
     *
     *  future modules: UDP I/O, filesystem, ..?
     *
     *  if any of these modules fail, the program exits with an error.
     */
    let quit: String = tokio::select! {
        term = terminal::terminal(&our_name, card_sender.clone(), print_receiver) => match term {
            Ok(_) => "graceful shutdown".to_string(),
            Err(e) => format!("exiting with error: {:?}", e),
        },
        _ = kernel::kernel(&our_name, peers.clone(), print_sender.clone(), card_receiver) => {
            "card sender died".to_string()
        },
        _ = websockets::ws_listener(card_sender.clone(), print_sender.clone(), tcp_listener) => {
            "websocket server died".to_string()
        }
    };

    println!("{}", quit);
}
