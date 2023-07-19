use ring::signature;
use ring::signature::KeyPair;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use tokio::sync::mpsc;

use ethers::prelude::*;

use crate::types::*;

mod engine;
mod filesystem;
mod http_server;
mod microkernel;
mod terminal;
mod types;
mod websockets;

const EVENT_LOOP_CHANNEL_CAPACITY: usize = 10_000;
const TERMINAL_CHANNEL_CAPACITY: usize = 32;
const WEBSOCKET_SENDER_CHANNEL_CAPACITY: usize = 100;
const FILESYSTEM_CHANNEL_CAPACITY: usize = 32;
const HTTP_CHANNEL_CAPACITY: usize = 32;

#[tokio::main]
async fn main() {
    // For use with https://github.com/tokio-rs/console
    // console_subscriber::init();

    let args: Vec<String> = env::args().collect();
    let our_name: String = args[1].clone();

    // kernel receives system messages via this channel, all other modules send messages
    let (kernel_message_sender, kernel_message_receiver): (MessageSender, MessageReceiver) =
        mpsc::channel(EVENT_LOOP_CHANNEL_CAPACITY);
    // websocket sender receives send messages via this channel, kernel send messages
    let (wss_message_sender, wss_message_receiver): (MessageSender, MessageReceiver) =
        mpsc::channel(WEBSOCKET_SENDER_CHANNEL_CAPACITY);
    // filesystem receives request messages via this channel, kernel sends messages
    let (fs_message_sender, fs_message_receiver): (MessageSender, MessageReceiver) =
        mpsc::channel(FILESYSTEM_CHANNEL_CAPACITY);
    // http server channel (eyre)
    let (http_server_sender, http_server_receiver): (MessageSender, MessageReceiver) =
        mpsc::channel(HTTP_CHANNEL_CAPACITY);
    // terminal receives prints via this channel, all other modules send prints
    let (print_sender, print_receiver): (PrintSender, PrintReceiver) =
        mpsc::channel(TERMINAL_CHANNEL_CAPACITY);

    // this will be replaced with actual chain reading from indexer module?
    let blockchain =
        std::fs::File::open("blockchain.json").expect("couldn't read from the chain lolz");
    let json: serde_json::Value =
        serde_json::from_reader(blockchain).expect("blockchain.json should be proper JSON");
    let pki: OnchainPKI = Arc::new(
        serde_json::from_value::<HashMap<String, Identity>>(json)
            .expect("should be a JSON map of identities"),
    );
    // our identity in the uqbar PKI
    let our = pki.get(&our_name).expect("we should be in the PKI").clone();

    // fake local blockchain
    let uqchain = engine::UqChain::new();
    let my_txn: engine::Transaction = engine::Transaction {
        from: our.address,
        signature: None,
        to: "0x0000000000000000000000000000000000000000000000000000000000005678"
            .parse()
            .unwrap(),
        town_id: 0,
        calldata: serde_json::to_value("hi").unwrap(),
        nonce: U256::from(1),
        gas_price: U256::from(0),
        gas_limit: U256::from(0),
    };
    let _ = uqchain.run_batch(vec![my_txn]);

    // this will be replaced with a key manager module
    let name_seed: [u8; 32] = our.address.into();
    let networking_keypair = signature::Ed25519KeyPair::from_seed_unchecked(&name_seed).unwrap();
    let hex_pubkey = hex::encode(networking_keypair.public_key().as_ref());
    println!("our networking public key: {}", hex_pubkey);
    assert!(hex_pubkey == our.networking_key);

    /*  we are currently running 4 I/O modules:
     *      terminal,
     *      websockets,
     *      filesystem,
     *      http-server,
     *  the kernel module will handle our userspace processes and receives
     *  all "messages", the basic message format for uqbar.
     *
     *  future modules: UDP I/O, ..?
     *
     *  if any of these modules fail, the program exits with an error.
     */
    let quit: String = tokio::select! {
        term = terminal::terminal(
            &our,
            kernel_message_sender.clone(),
            print_receiver,
        ) => match term {
            Ok(_) => "graceful shutdown".to_string(),
            Err(e) => format!("exiting with error: {:?}", e),
        },
        _ = microkernel::kernel(
            &our,
            kernel_message_sender.clone(),
            print_sender.clone(),
            kernel_message_receiver,
            wss_message_sender.clone(),
            fs_message_sender.clone(),
            http_server_sender.clone(),
        ) => { "microkernel died".to_string() },
        _ = websockets::websockets(
            our.clone(),
            networking_keypair,
            pki.clone(),
            wss_message_receiver,
            wss_message_sender.clone(),
            kernel_message_sender.clone(),
            print_sender.clone(),
        ) => { "websocket sender died".to_string() },
        _ = filesystem::fs_sender(
            &our_name,
            kernel_message_sender.clone(),
            print_sender.clone(),
            fs_message_receiver
        ) => { "".to_string() },
        _ = http_server::http_server(
            &our_name,
            http_server_receiver,
            http_server_sender.clone(),
            print_sender.clone(),
        ) => { "http_server died".to_string() },
    };

    println!("{}", quit);
}
