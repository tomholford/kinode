use ring::signature;
use ring::signature::KeyPair;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::types::*;

mod engine;
mod filesystem;
mod microkernel;
mod terminal;
mod types;
mod ws;
mod keygen;

const EVENT_LOOP_CHANNEL_CAPACITY: usize = 10_000;
const EVENT_LOOP_DEBUG_CHANNEL_CAPACITY: usize = 50;
const TERMINAL_CHANNEL_CAPACITY: usize = 32;
const WEBSOCKET_SENDER_CHANNEL_CAPACITY: usize = 100;
const FILESYSTEM_CHANNEL_CAPACITY: usize = 32;
const KEYGEN_CHANNEL_CAPACITY: usize = 32;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() {
    // For use with https://github.com/tokio-rs/console
    // console_subscriber::init();

    let args: Vec<String> = env::args().collect();
    let process_manager_wasm_path = args[1].clone();
    let home_directory_path = &args[2];
    let our_name: String = args[3].clone();

    // kernel receives system messages via this channel, all other modules send messages
    let (kernel_message_sender, kernel_message_receiver): (MessageSender, MessageReceiver) =
        mpsc::channel(EVENT_LOOP_CHANNEL_CAPACITY);
    // kernel receives debug messages via this channel, terminal sends messages
    let (kernel_debug_message_sender, kernel_debug_message_receiver): (DebugSender, DebugReceiver) =
        mpsc::channel(EVENT_LOOP_DEBUG_CHANNEL_CAPACITY);
    // websocket sender receives send messages via this channel, kernel send messages
    let (wss_message_sender, wss_message_receiver): (MessageSender, MessageReceiver) =
        mpsc::channel(WEBSOCKET_SENDER_CHANNEL_CAPACITY);
    // filesystem receives request messages via this channel, kernel sends messages
    let (fs_message_sender, fs_message_receiver): (MessageSender, MessageReceiver) =
        mpsc::channel(FILESYSTEM_CHANNEL_CAPACITY);
    let (keygen_message_sender, keygen_message_receiver): (MessageSender, MessageReceiver) =
        mpsc::channel(KEYGEN_CHANNEL_CAPACITY);
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
    // let uqchain = engine::UqChain::new();
    // let my_txn: engine::Transaction = engine::Transaction {
    //     from: our.address,
    //     signature: None,
    //     to: "0x0000000000000000000000000000000000000000000000000000000000005678"
    //         .parse()
    //         .unwrap(),
    //     town_id: 0,
    //     calldata: serde_json::to_value("hi").unwrap(),
    //     nonce: U256::from(1),
    //     gas_price: U256::from(0),
    //     gas_limit: U256::from(0),
    // };
    // let _ = uqchain.run_batch(vec![my_txn]);

    // this will be replaced with a key manager module
    let name_seed: [u8; 32] = our.address.into();
    let networking_keypair = signature::Ed25519KeyPair::from_seed_unchecked(&name_seed).unwrap();
    let hex_pubkey = hex::encode(networking_keypair.public_key().as_ref());
    assert!(hex_pubkey == our.networking_key);

    let _ = print_sender
        .send(format!("{}.. now online", our_name))
        .await;
    let _ = print_sender
        .send(format!("our networking public key: {}", hex_pubkey))
        .await;

    /*  we are currently running 4 I/O modules:
     *      terminal,
     *      websockets,
     *      filesystem,
     *      keygen
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
            VERSION,
            kernel_message_sender.clone(),
            kernel_debug_message_sender,
            print_receiver,
        ) => match term {
            Ok(_) => "graceful shutdown".to_string(),
            Err(e) => format!("exiting with error: {:?}", e),
        },
        _ = microkernel::kernel(
            &our,
            process_manager_wasm_path,
            kernel_message_sender.clone(),
            print_sender.clone(),
            kernel_message_receiver,
            kernel_debug_message_receiver,
            wss_message_sender.clone(),
            fs_message_sender.clone(),
            keygen_message_sender.clone(),
        ) => { "microkernel died".to_string() },
        _ = ws::websockets(
            our.clone(),
            networking_keypair,
            pki.clone(),
            kernel_message_sender.clone(),
            print_sender.clone(),
            wss_message_receiver,
            wss_message_sender.clone(),
        ) => { "websocket sender died".to_string() },
        _ = filesystem::fs_sender(
            &our_name,
            home_directory_path,
            kernel_message_sender.clone(),
            print_sender.clone(),
            fs_message_receiver
        ) => { "".to_string() },
        _ = keygen::keygen(
            &our_name,
            kernel_message_sender.clone(),
            keygen_message_receiver,
            print_sender.clone(),
        ) => { "keygen died".to_string() },
    };
    println!("");
    println!("\x1b[38;5;196m{}\x1b[0m", quit);
}
