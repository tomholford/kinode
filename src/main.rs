use std::{collections::HashMap, sync::Arc};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, RwLock};
use std::env;

use ethers::prelude::*;

use crate::types::*;

mod types;
mod terminal;
mod websockets;
mod microkernel;
mod blockchain;
mod engine;

const EVENT_LOOP_CHANNEL_CAPACITY: usize = 10_000;
const TERMINAL_CHANNEL_CAPACITY: usize = 32;
const WEBSOCKET_SENDER_CHANNEL_CAPACITY: usize = 100;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    let our_name: String = args[1].clone();

    // kernel receives system cards via this channel, all other modules send cards
    let (kernel_card_sender, kernel_card_receiver): (CardSender, CardReceiver) = mpsc::channel(EVENT_LOOP_CHANNEL_CAPACITY);
    // websocket sender receives send cards via this channel, kernel send cards
    let (wss_card_sender, wss_card_receiver): (CardSender, CardReceiver) = mpsc::channel(WEBSOCKET_SENDER_CHANNEL_CAPACITY);
    // terminal receives prints via this channel, all other modules send prints
    let (print_sender, print_receiver): (PrintSender, PrintReceiver) = mpsc::channel(TERMINAL_CHANNEL_CAPACITY);

    let uqchain = engine::UqChain::new();
    let my_txn: engine::Transaction = engine::Transaction {
                                        from: "0x0000000000000000000000000000000000000000000000000000000000001234".parse().unwrap(),
                                        signature: None,
                                        to: "0x0000000000000000000000000000000000000000000000000000000000005678".parse().unwrap(),
                                        town_id: 0,
                                        calldata: serde_json::to_value("hi").unwrap(),
                                        nonce: U256::from(1),
                                        gas_price: U256::from(0),
                                        gas_limit: U256::from(0),
                                    };
    let _ = uqchain.run_batch(vec![my_txn]);

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
        term = terminal::terminal(&our_name, kernel_card_sender.clone(), print_receiver) => match term {
            Ok(_) => "graceful shutdown".to_string(),
            Err(e) => format!("exiting with error: {:?}", e),
        },
        _ = microkernel::kernel(&our_name, kernel_card_sender.clone(), print_sender.clone(), kernel_card_receiver, wss_card_sender.clone()) => {
            "microkernel died".to_string()
        },
        _ = websockets::ws_listener(kernel_card_sender.clone(), print_sender.clone(), tcp_listener) => {
            "websocket listener died".to_string()
        },
        _ = websockets::ws_sender(peers.clone(), print_sender.clone(), wss_card_receiver) => {
            "websocket sender died".to_string()
        }
    };

    println!("{}", quit);
}
