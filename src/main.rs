use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    Aes256Gcm, Key,
};
use anyhow::Result;
use lazy_static::__Deref;
use reqwest;
use ring::pbkdf2;
use ring::pkcs8::Document;
use ring::signature::{self, KeyPair};
use std::env;
use std::num::NonZeroU32;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio::{fs, time::timeout};

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
mod encryptor;

const EVENT_LOOP_CHANNEL_CAPACITY: usize = 10_000;
const EVENT_LOOP_DEBUG_CHANNEL_CAPACITY: usize = 50;
const TERMINAL_CHANNEL_CAPACITY: usize = 32;
const WEBSOCKET_SENDER_CHANNEL_CAPACITY: usize = 100_000;
const FILESYSTEM_CHANNEL_CAPACITY: usize = 32;
const HTTP_CHANNEL_CAPACITY: usize = 32;
const HTTP_CLIENT_CHANNEL_CAPACITY: usize = 32;
const ETH_RPC_CHANNEL_CAPACITY: usize = 32;
const ENCRYPTOR_CHANNEL_CAPACITY: usize = 32;

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
    // http server channel w/ websockets (eyre)
    let (http_server_sender, http_server_receiver): (MessageSender, MessageReceiver) =
        mpsc::channel(HTTP_CHANNEL_CAPACITY);
    // http client performs http requests on behalf of processes
    let (eth_rpc_sender, eth_rpc_receiver): (MessageSender, MessageReceiver) =
        mpsc::channel(ETH_RPC_CHANNEL_CAPACITY);
    let (http_client_sender, http_client_receiver): (MessageSender,MessageReceiver) =
        mpsc::channel(HTTP_CLIENT_CHANNEL_CAPACITY);
    // encryptor handles end-to-end encryption for client messages
    let (encryptor_sender, encryptor_receiver): (MessageSender,MessageReceiver ) =
        mpsc::channel(ENCRYPTOR_CHANNEL_CAPACITY);
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
    let http_server_port = http_server::find_open_port(8080).await.unwrap();
    let (kill_tx, kill_rx) = oneshot::channel::<bool>();
    let keyfile = fs::read(format!("{}/.network.keys", home_directory_path)).await;

    let (our, networking_keypair, jwt_secret_bytes): (
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

        let (tx, mut rx) = mpsc::channel::<(Registration, Document, Vec<u8>)>(1);
        let (registration, serialized_networking_keypair, jwt_secret_bytes) = tokio::select! {
            _ = register::register(tx, kill_rx, http_server_port, http_server_port)
                => panic!("registration failed"),
            (registration, serialized_networking_keypair, jwt_secret_bytes) = async {
                while let Some(fin) = rx.recv().await {
                    return fin
                }
                panic!("registration failed")
            } => (registration, serialized_networking_keypair, jwt_secret_bytes),
        };

        // TODO move all this logic into register::register
        // TODO: if IP is localhost, assign a router...
        let networking_keypair =
            signature::Ed25519KeyPair::from_pkcs8(serialized_networking_keypair.as_ref()).unwrap();
        let hex_pubkey = hex::encode(networking_keypair.public_key().as_ref());
        let ws_port = http_server::find_open_port(9000).await.unwrap();
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

        let jwtciphertext: Vec<u8> = cipher.encrypt(&nonce, jwt_secret_bytes.as_ref()).unwrap();

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

        println!("registration complete!");
        (our, networking_keypair, jwt_secret_bytes)
    };

    //  bootstrap FS.
    let _ = print_sender
        .send(Printout {
            verbosity: 0,
            content: "bootstrapping fs...".to_string(),
        })
        .await;

    let (kernel_process_map, manifest, wal, fs_directory) =
        lfs::bootstrap(home_directory_path.clone())
            .await
            .expect("fs bootstrap failed!");

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

    /*
     *  the kernel module will handle our userspace processes and receives
     *  all "messages", the basic message format for uqbar.
     *
     *  if any of these modules fail, the program exits with an error.
     */
    let networking_keypair_arc = Arc::new(networking_keypair);

    let mut tasks = tokio::task::JoinSet::<Result<()>>::new();
    tasks.spawn(kernel::kernel(
        our.clone(),
        home_directory_path.into(),
        kernel_process_map,
        kernel_message_sender.clone(),
        print_sender.clone(),
        kernel_message_receiver,
        network_error_receiver,
        kernel_debug_message_receiver,
        net_message_sender.clone(),
        fs_message_sender,
        lfs_message_sender,
        http_server_sender,
        http_client_sender,
        eth_rpc_sender,
        encryptor_sender,
    ));
    tasks.spawn(net::networking(
        our.clone(),
        our_ip,
        networking_keypair_arc.clone(),
        kernel_message_sender.clone(),
        network_error_sender,
        print_sender.clone(),
        net_message_receiver,
    ));
    tasks.spawn(filesystem::fs_sender(
        our.name.clone(),
        home_directory_path.into(),
        kernel_message_sender.clone(),
        print_sender.clone(),
        fs_message_receiver,
    ));
    tasks.spawn(lfs::fs_sender(
        our.name.clone(),
        fs_directory,
        manifest,
        wal,
        kernel_message_sender.clone(),
        print_sender.clone(),
        lfs_message_receiver,
    ));
    tasks.spawn(http_server::http_server(
        our.name.clone(),
        http_server_port,
        jwt_secret_bytes.clone(),
        http_server_receiver,
        kernel_message_sender.clone(),
        print_sender.clone(),
    ));
    tasks.spawn(http_client::http_client(
        our.name.clone(),
        kernel_message_sender.clone(),
        http_client_receiver,
        print_sender.clone(),
    ));
    tasks.spawn(eth_rpc::eth_rpc(
        our.name.clone(),
        rpc_url.clone(),
        kernel_message_sender.clone(),
        eth_rpc_receiver,
        print_sender.clone(),
    ));
    tasks.spawn(encryptor::encryptor(
        our.name.clone(),
        networking_keypair_arc.clone(),
        kernel_message_sender.clone(),
        encryptor_receiver,
        print_sender.clone(),
    ));
    // if a runtime task exits, try to recover it,
    // unless it was terminal signaling a quit
    let quit_msg: String = tokio::select! {
        Some(res) = tasks.join_next() => {
            if let Err(e) = res {
                format!("what does this mean? {:?}", e)
            } else if let Ok(Err(e)) = res {
                format!(
                    "\x1b[38;5;196muh oh, a kernel process crashed: {}\x1b[0m",
                    e
                )
                // TODO restart the task
            } else {
                format!("what does this mean???")
                // TODO restart the task
            }
        }
        quit = terminal::terminal(
            our.clone(),
            VERSION,
            home_directory_path.into(),
            kernel_message_sender.clone(),
            kernel_debug_message_sender,
            print_sender.clone(),
            print_receiver,
        ) => {
            match quit {
                Ok(_) => "graceful exit".into(),
                Err(e) => e.to_string(),
            }
        }
    };
    // gracefully abort all running processes in kernel
    let _ = kernel_message_sender
        .send(KernelMessage {
            id: 0,
            source: Address {
                node: our.name.clone(),
                process: ProcessId::Name("kernel".into()),
            },
            target: Address {
                node: our.name.clone(),
                process: ProcessId::Name("kernel".into()),
            },
            rsvp: None,
            message: Message::Request(Request {
                inherit: false,
                expects_response: false,
                ipc: Some(serde_json::to_string(&KernelCommand::Shutdown).unwrap()),
                metadata: None,
            }),
            payload: None,
        })
        .await;
    // abort all remaining tasks
    tasks.shutdown().await;
    let _ = crossterm::terminal::disable_raw_mode();
    println!("");
    println!("\x1b[38;5;196m{}\x1b[0m", quit_msg);
    return;
}
