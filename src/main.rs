use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    Aes256Gcm, Key,
};
use lazy_static::__Deref;
use reqwest;
use ring::pbkdf2;
use ring::pkcs8::Document;
use ring::signature::{self, KeyPair};
use std::env;
use std::num::NonZeroU32;
use tokio::sync::{mpsc, oneshot};
use tokio::{fs, time::timeout};

use crate::http_server::find_open_port;
use crate::register::{DISK_KEY_SALT, ITERATIONS};
use crate::types::*;

mod eth_rpc;
mod filesystem;
mod http_client;
mod http_server;
mod kernel;
mod lfs;
mod net;
mod register;
mod terminal;
mod types;

const EVENT_LOOP_CHANNEL_CAPACITY: usize = 10_000;
const EVENT_LOOP_DEBUG_CHANNEL_CAPACITY: usize = 50;
const TERMINAL_CHANNEL_CAPACITY: usize = 32;
const WEBSOCKET_SENDER_CHANNEL_CAPACITY: usize = 100_000;
const FILESYSTEM_CHANNEL_CAPACITY: usize = 32;
const HTTP_CHANNEL_CAPACITY: usize = 32;
const HTTP_CLIENT_CHANNEL_CAPACITY: usize = 32;
const ETH_RPC_CHANNEL_CAPACITY: usize = 32;

const VERSION: &str = env!("CARGO_PKG_VERSION");

static PBKDF2_ALG: pbkdf2::Algorithm = pbkdf2::PBKDF2_HMAC_SHA256; // TODO maybe look into Argon2

#[tokio::main]
async fn main() {
    // For use with https://github.com/tokio-rs/console
    // console_subscriber::init();

    // DEMO ONLY: remove all CLI arguments
    let args: Vec<String> = env::args().collect();
    let home_directory_path = &args[1];
    // let home_directory_path = "home";
    // create home directory if it does not already exist
    if let Err(e) = fs::create_dir_all(home_directory_path).await {
        panic!("failed to create home directory: {:?}", e);
    }
    // read PKI from HTTP endpoint served by RPC
    let rpc_url = &args[2];

    // kernel receives system messages via this channel, all other modules send messages
    let (kernel_message_sender, kernel_message_receiver): (MessageSender, MessageReceiver) =
        mpsc::channel(EVENT_LOOP_CHANNEL_CAPACITY);
    // networking module sends error messages to kernel
    let (network_error_sender, network_error_receiver): (NetworkErrorSender, NetworkErrorReceiver) =
        mpsc::channel(EVENT_LOOP_CHANNEL_CAPACITY);
    // kernel receives debug messages via this channel, terminal sends messages
    let (kernel_debug_message_sender, kernel_debug_message_receiver): (DebugSender, DebugReceiver) =
        mpsc::channel(EVENT_LOOP_DEBUG_CHANNEL_CAPACITY);
    // websocket sender receives send messages via this channel, kernel send messages
    let (net_message_sender, net_message_receiver): (MessageSender, MessageReceiver) =
        mpsc::channel(WEBSOCKET_SENDER_CHANNEL_CAPACITY);
    // filesystem receives request messages via this channel, kernel sends messages
    let (fs_message_sender, fs_message_receiver): (MessageSender, MessageReceiver) =
        mpsc::channel(FILESYSTEM_CHANNEL_CAPACITY.clone());
    // new FS channel: todo merge
    let (lfs_message_sender, lfs_message_receiver): (MessageSender, MessageReceiver) =
        mpsc::channel(FILESYSTEM_CHANNEL_CAPACITY);
    // http server channel (eyre)
    let (http_server_sender, http_server_receiver): (MessageSender, MessageReceiver) =
        mpsc::channel(HTTP_CHANNEL_CAPACITY);
    // http client performs http requests on behalf of processes
    let (http_client_message_sender, http_client_message_receiver): (
        MessageSender,
        MessageReceiver,
    ) = mpsc::channel(HTTP_CLIENT_CHANNEL_CAPACITY);
    let (eth_rpc_sender, eth_rpc_receiver): (MessageSender, MessageReceiver) =
        mpsc::channel(ETH_RPC_CHANNEL_CAPACITY);
    // terminal receives prints via this channel, all other modules send prints
    let (print_sender, print_receiver): (PrintSender, PrintReceiver) =
        mpsc::channel(TERMINAL_CHANNEL_CAPACITY);

    println!("finding public IP address...");
    let our_ip = {
        if let Ok(Some(ip)) = timeout(std::time::Duration::from_secs(5), public_ip::addr_v4()).await
        {
            ip.to_string()
        } else {
            println!(
                "\x1b[38;5;196mfailed to find public IPv4 address: booting as a routed node\x1b[0m"
            );
            "localhost".into()
        }
    };

    // check if we have keys saved on disk, encrypted
    // if so, prompt user for "password" to decrypt with

    // once password is received, use to decrypt local keys file,
    // and pass the keys into boot process as is done in registration.

    // NOTE: when we log in, we MUST check the PKI to make sure our
    // information matches what we think it should be. this includes
    // username, address, networking key, and routing info.
    // if any do not match, we should prompt user to create a "transaction"
    // that updates their PKI info on-chain.
    let http_server_port = find_open_port(8080).await.unwrap();
    let (kill_tx, kill_rx) = oneshot::channel::<bool>();
    let keyfile = fs::read(format!("{}/.network.keys", home_directory_path)).await;

    let (our, networking_keypair, _jwt_secret_bytes): (
        Identity,
        signature::Ed25519KeyPair,
        Vec<u8>,
    ) = if keyfile.is_ok() {
        // LOGIN flow
        // get username, keyfile, and jwt_secret from disk
        let (username, key, jwt_secret) =
            bincode::deserialize::<(String, Vec<u8>, Vec<u8>)>(&keyfile.unwrap()).unwrap();

        println!(
            "\u{1b}]8;;{}\u{1b}\\{}\u{1b}]8;;\u{1b}\\",
            format!("http://localhost:{}/login", http_server_port),
            format!(
                "Welcome back, {}. Click here to log in to your node.",
                username
            ),
        );
        println!("(http://localhost:{}/login)", http_server_port);
        if our_ip != "localhost" {
            println!(
                "(if on a remote machine: http://{}:{}/login)",
                our_ip, http_server_port
            );
        }

        let (tx, mut rx) = mpsc::channel::<(signature::Ed25519KeyPair, Vec<u8>)>(1);
        let (networking_keypair, jwt_secret_bytes) = tokio::select! {
            _ = register::login(
                tx,
                kill_rx,
                key,
                jwt_secret,
                http_server_port,
                &username
            ) => panic!("login failed"),
            (networking_keypair, jwt_secret_bytes) = async {
                while let Some(fin) = rx.recv().await {
                    return fin
                }
                panic!("login failed")
            } => (networking_keypair, jwt_secret_bytes),
        };

        // check if Identity for this username has correct networking keys,
        // if not, prompt user to reset them.
        // TODO this should be decrypted from disk
        let our_identity = Identity {
            name: username.clone(),
            address: "0x0".into(),
            networking_key: hex::encode(networking_keypair.public_key().as_ref()),
            ws_routing: None,
            allowed_routers: vec![],
        };

        (our_identity.clone(), networking_keypair, jwt_secret_bytes)
    } else {
        // REGISTER flow
        println!(
            "\u{1b}]8;;{}\u{1b}\\{}\u{1b}]8;;\u{1b}\\",
            format!("http://localhost:{}/register", http_server_port),
            "Click here to register your node.",
        );
        println!("(http://localhost:{}/register)", http_server_port);
        if our_ip != "localhost" {
            println!(
                "(if on a remote machine: http://{}:{}/register)",
                our_ip, http_server_port
            );
        }

        let (tx, mut rx) = mpsc::channel::<(Registration, Document, Vec<u8>, String)>(1);
        let (registration, serialized_networking_keypair, jwt_secret_bytes, signature) = tokio::select! {
            _ = register::register(tx, kill_rx, http_server_port, http_server_port)
                => panic!("registration failed"),
            (registration, serialized_networking_keypair, jwt_secret_bytes, signature) = async {
                while let Some(fin) = rx.recv().await {
                    return fin
                }
                panic!("registration failed")
            } => (registration, serialized_networking_keypair, jwt_secret_bytes, signature),
        };

        println!("generating disk encryption keys...");
        let mut disk_key: DiskKey = [0u8; CREDENTIAL_LEN];
        pbkdf2::derive(
            PBKDF2_ALG,
            NonZeroU32::new(ITERATIONS).unwrap(),
            DISK_KEY_SALT,
            registration.password.as_bytes(),
            &mut disk_key,
        );
        println!(
            "saving encrypted networking keys to {}/.network.keys",
            home_directory_path
        );
        let key = Key::<Aes256Gcm>::from_slice(&disk_key);
        let cipher = Aes256Gcm::new(&key);
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng); // 96-bits; unique per message
        let ciphertext: Vec<u8> = cipher
            .encrypt(&nonce, serialized_networking_keypair.as_ref())
            .unwrap();
        let networking_keypair =
            signature::Ed25519KeyPair::from_pkcs8(serialized_networking_keypair.as_ref()).unwrap();

        let jwtciphertext: Vec<u8> = cipher.encrypt(&nonce, jwt_secret_bytes.as_ref()).unwrap();

        // TODO: if IP is localhost, assign a router...
        let hex_pubkey = hex::encode(networking_keypair.public_key().as_ref());
        let ws_port = find_open_port(9000).await.unwrap();
        let our = Identity {
            name: registration.username.clone(),
            address: registration.address.clone(),
            networking_key: hex_pubkey.clone(),
            ws_routing: if our_ip == "localhost" || !registration.direct {
                None
            } else {
                Some((our_ip.clone(), ws_port))
            },
            allowed_routers: if our_ip == "localhost" || !registration.direct {
                vec!["rolr1".into(), "rolr2".into(), "rolr3".into()]
            } else {
                vec![]
            },
        };

        let id_transaction = IdentityTransaction {
            from: registration.address.clone(),
            signature: Some(signature),
            to: "0x0".into(),
            town_id: 0,
            calldata: our.clone(),
            nonce: "0".into(),
        };

        // make POST
        // TODO send transaction to actual RPC endpoint
        // let response = reqwest::Client::new()
        //     .post(rpc_url)
        //     .body(bincode::serialize(&id_transaction).unwrap())
        //     .send()
        //     .await
        //     .unwrap();

        // assert!(response.status().is_success());

        println!("\"posting\" \"transaction\" to \"blockchain\"...");
        std::thread::sleep(std::time::Duration::from_secs(5));
        fs::write(
            format!("{}/.network.keys", home_directory_path),
            bincode::serialize(&(
                registration.username.clone(),
                [nonce.deref().to_vec(), ciphertext].concat(),
                [nonce.deref().to_vec(), jwtciphertext].concat(),
            ))
            .unwrap(),
        )
        .await
        .unwrap();

        //  load in initial boot sequence
        // let mut boot_sequence_path = "boot_sequence.bin";
        // for i in 3..args.len() - 1 {
        //     if args[i] == "--bs" {
        //         boot_sequence_path = &args[i + 1];
        //         break;
        //     }
        // }
        // let boot_sequence = match fs::read(boot_sequence_path).await {
        //     Ok(boot_sequence) => match bincode::deserialize::<Vec<BootOutboundRequest>>(&boot_sequence) {
        //         Ok(bs) => bs,
        //         Err(e) => panic!("failed to deserialize boot sequence: {:?}", e),
        //     },
        //     Err(e) => panic!("failed to read boot sequence, try running `cargo run` in the boot_sequence directory: {:?}", e),
        // };

        // just put the boot sequence here?
        // save_bytes(&home_directory_path, "terminal").await;
        // save_bytes(&home_directory_path, "sequentialize").await;

        let kernel_address = Address {
            node: our.name.clone(),
            process: ProcessId::Name("kernel".into()),
        };

        // send jwt_secret to http_bindings
        kernel_message_sender
            .send(KernelMessage {
                id: rand::random(),
                source: kernel_address.clone(),
                target: Address {
                    node: our.name.clone(),
                    process: ProcessId::Name("sequentialize".into()),
                },
                rsvp: None,
                message: Message::Request(Request {
                    inherit: false,
                    expects_response: false,
                    ipc: Some(
                        serde_json::to_string(&SequentializeRequest::QueueMessage(QueueMessage {
                            target: ProcessId::Name("http_bindings".into()),
                            request: Request {
                                inherit: false,
                                expects_response: false,
                                ipc: Some(
                                    serde_json::to_string(
                                        &serde_json::json!({"action": "set-jwt-secret"}),
                                    )
                                    .unwrap(),
                                ),
                                metadata: None,
                            },
                        }))
                        .unwrap(),
                    ),
                    metadata: None,
                }),
                payload: Some(Payload {
                    mime: None,
                    bytes: jwt_secret_bytes.to_vec(),
                }),
            })
            .await
            .unwrap();

        // run sequentialize queue
        kernel_message_sender
            .send(KernelMessage {
                id: rand::random(),
                source: kernel_address.clone(),
                target: Address {
                    node: our.name.clone(),
                    process: ProcessId::Name("sequentialize".into()),
                },
                rsvp: None,
                message: Message::Request(Request {
                    inherit: false,
                    expects_response: false,
                    ipc: Some(serde_json::to_string(&SequentializeRequest::RunQueue).unwrap()),
                    metadata: None,
                }),
                payload: None,
            })
            .await
            .unwrap();

        println!("registration complete!");
        (our, networking_keypair, jwt_secret_bytes)
    };

    let _ = kill_tx.send(true);
    let _ = print_sender
        .send(Printout {
            verbosity: 0,
            content: format!("{}.. now online", our.name),
        })
        .await;
    let _ = print_sender
        .send(Printout {
            verbosity: 0,
            content: format!("our networking public key: {}", our.networking_key),
        })
        .await;

    // at boot, always save username to disk for login
    fs::write(format!("{}/.user", home_directory_path), our.name.clone())
        .await
        .unwrap();

    /*
     *  the kernel module will handle our userspace processes and receives
     *  all "messages", the basic message format for uqbar.
     *
     *  if any of these modules fail, the program exits with an error.
     */

    let kernel_handle = tokio::spawn(kernel::kernel(
        our.clone(),
        kernel_message_sender.clone(),
        print_sender.clone(),
        kernel_message_receiver,
        network_error_receiver,
        kernel_debug_message_receiver,
        net_message_sender.clone(),
        fs_message_sender,
        lfs_message_sender,
        http_server_sender,
        http_client_message_sender,
        eth_rpc_sender,
    ));
    let net_handle = tokio::spawn(net::networking(
        our.clone(),
        our_ip,
        networking_keypair,
        kernel_message_sender.clone(),
        network_error_sender,
        print_sender.clone(),
        net_message_receiver,
    ));
    let fs_handle = tokio::spawn(filesystem::fs_sender(
        our.name.clone(),
        home_directory_path.into(),
        kernel_message_sender.clone(),
        print_sender.clone(),
        fs_message_receiver,
    ));
    let lfs_handle = tokio::spawn(lfs::fs_sender(
        our.name.clone(),
        home_directory_path.into(),
        kernel_message_sender.clone(),
        print_sender.clone(),
        lfs_message_receiver,
    ));
    let http_server_handle = tokio::spawn(http_server::http_server(
        our.name.clone(),
        http_server_port,
        http_server_receiver,
        kernel_message_sender.clone(),
        print_sender.clone(),
    ));
    let http_client_handle = tokio::spawn(http_client::http_client(
        our.name.clone(),
        kernel_message_sender.clone(),
        http_client_message_receiver,
        print_sender.clone(),
    ));
    let eth_rpc_handle = tokio::spawn(eth_rpc::eth_rpc(
        our.name.clone(),
        rpc_url.clone(),
        kernel_message_sender.clone(),
        eth_rpc_receiver,
        print_sender.clone(),
    ));
    // TODO: abort all these so graceful shutdown doesn't look like a crash
    let quit = tokio::select! {
        term = terminal::terminal(
            &our,
            VERSION,
            home_directory_path.into(),
            kernel_message_sender.clone(),
            kernel_debug_message_sender,
            print_sender.clone(),
            print_receiver,
        ) => match term {
            Ok(_) => "graceful shutdown".to_string(),
            Err(e) => format!("exiting with error: {:?}", e),
        },
        _ = kernel_handle      => {"fatal: kernel exit".into()},
        _ = net_handle         => {"fatal: net exit".into()},
        _ = fs_handle          => {"fatal: fs exit".into()},
        _ = http_server_handle => {"fatal: http_server exit".into()},
        _ = http_client_handle => {"fatal: http_client exit".into()},
        _ = eth_rpc_handle     => {"fatal: eth_rpc exit".into()},
        _ = lfs_handle         => {"fatal: lfs exit".into()},
    };
    let _ = crossterm::terminal::disable_raw_mode();
    println!("");
    println!("\x1b[38;5;196m{}\x1b[0m", quit);
}
