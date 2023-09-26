use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    Aes256Gcm, Key,
};
use anyhow::Result;
use ethers::prelude::{abigen, namehash, Address as EthAddress, Provider, U256};
use ethers_providers::Ws;
use lazy_static::__Deref;
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

mod encryptor;
mod eth_rpc;
mod filesystem;
mod http_client;
mod http_server;
mod kernel;
mod net;
mod register;
mod terminal;
mod types;
mod vfs;

const EVENT_LOOP_CHANNEL_CAPACITY: usize = 10_000;
const EVENT_LOOP_DEBUG_CHANNEL_CAPACITY: usize = 50;
const TERMINAL_CHANNEL_CAPACITY: usize = 32;
const WEBSOCKET_SENDER_CHANNEL_CAPACITY: usize = 100_000;
const FILESYSTEM_CHANNEL_CAPACITY: usize = 32;
const HTTP_CHANNEL_CAPACITY: usize = 32;
const HTTP_CLIENT_CHANNEL_CAPACITY: usize = 32;
const ETH_RPC_CHANNEL_CAPACITY: usize = 32;
const VFS_CHANNEL_CAPACITY: usize = 1_000;
const ENCRYPTOR_CHANNEL_CAPACITY: usize = 32;

const QNS_SEPOLIA_ADDRESS: &str = "0x9e5ed0e7873E0d7f10eEb6dE72E87fE087A12776";

const VERSION: &str = env!("CARGO_PKG_VERSION");

static PBKDF2_ALG: pbkdf2::Algorithm = pbkdf2::PBKDF2_HMAC_SHA256; // TODO maybe look into Argon2

abigen!(QNSRegistry, "src/QNSRegistry.json");

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
    // TODO this is so incredibly bad, lol, lmao
    let mut rpc_url =
        "wss://eth-sepolia.g.alchemy.com/v2/W0nka5SiRCHASxyF6jzJ7HkQaMfnq4Mh".to_string();

    for (i, arg) in args.iter().enumerate() {
        if arg == "--rpc" {
            // Check if the next argument exists and is not another flag
            if i + 1 < args.len() && !args[i + 1].starts_with('-') {
                rpc_url = args[i + 1].clone();
            }
        }
    }

    // kernel receives system messages via this channel, all other modules send messages
    let (kernel_message_sender, kernel_message_receiver): (MessageSender, MessageReceiver) =
        mpsc::channel(EVENT_LOOP_CHANNEL_CAPACITY);
    // kernel informs other runtime modules of capabilities through this
    let (caps_oracle_sender, caps_oracle_receiver) = mpsc::unbounded_channel::<CapMessage>();
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
    // http server channel w/ websockets (eyre)
    let (http_server_sender, http_server_receiver): (MessageSender, MessageReceiver) =
        mpsc::channel(HTTP_CHANNEL_CAPACITY);
    // http client performs http requests on behalf of processes
    let (eth_rpc_sender, eth_rpc_receiver): (MessageSender, MessageReceiver) =
        mpsc::channel(ETH_RPC_CHANNEL_CAPACITY);
    let (http_client_sender, http_client_receiver): (MessageSender, MessageReceiver) =
        mpsc::channel(HTTP_CLIENT_CHANNEL_CAPACITY);
    // vfs maintains metadata about files in fs for processes
    let (vfs_message_sender, vfs_message_receiver): (MessageSender, MessageReceiver) =
        mpsc::channel(VFS_CHANNEL_CAPACITY);
    // encryptor handles end-to-end encryption for client messages
    let (encryptor_sender, encryptor_receiver): (MessageSender, MessageReceiver) =
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
    // username, networking key, and routing info.
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
        let (username, routers, key, jwt_secret) =
            bincode::deserialize::<(String, Vec<String>, Vec<u8>, Vec<u8>)>(&keyfile.unwrap())
                .unwrap();

        println!(
            "\u{1b}]8;;{}\u{1b}\\{}\u{1b}]8;;\u{1b}\\",
            format!("http://localhost:{}/login", http_server_port),
            format!(
                "Welcome back, {}, Click here to log in to your node.",
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
                &username,
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
        let Ok(ws_rpc) = Provider::<Ws>::connect(rpc_url.clone()).await else {
            panic!("rpc: couldn't connect to blockchain wss endpoint");
        };
        let qns_address: EthAddress = QNS_SEPOLIA_ADDRESS.parse().unwrap();
        let contract = QNSRegistry::new(qns_address, ws_rpc.into());
        let node_id: U256 = namehash(&username).as_bytes().into();
        let onchain_id = contract.ws(node_id).call().await.unwrap(); // TODO unwrap

        // double check that routers match on-chain information
        let namehashed_routers: Vec<[u8; 32]> = routers
            .clone()
            .into_iter()
            .map(|name| {
                let hash = namehash(&name);
                let mut result = [0u8; 32];
                result.copy_from_slice(hash.as_bytes());
                result
            })
            .collect();

        // double check that keys match on-chain information
        if (
            onchain_id.routers != namehashed_routers
                || onchain_id.public_key != networking_keypair.public_key().as_ref()
            // || (onchain_id.ip_and_port > 0 && onchain_id.ip_and_port != combineIpAndPort(
            //     our_ip.clone(),
            //     http_server_port,
            // ))
        ) {
            panic!("CRITICAL: your routing information does not match on-chain records");
        }

        let our_identity = Identity {
            name: username.clone(),
            networking_key: format!(
                "0x{}",
                hex::encode(networking_keypair.public_key().as_ref())
            ),
            ws_routing: if onchain_id.ip > 0 && onchain_id.port > 0 {
                let ip = format!(
                    "{}.{}.{}.{}",
                    (onchain_id.ip >> 24) & 0xFF,
                    (onchain_id.ip >> 16) & 0xFF,
                    (onchain_id.ip >> 8) & 0xFF,
                    onchain_id.ip & 0xFF
                );
                Some((ip, onchain_id.port))
            } else {
                None
            },
            allowed_routers: routers,
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

        let (tx, mut rx) = mpsc::channel::<(Identity, String, Document, Vec<u8>)>(1);
        let (our, password, serialized_networking_keypair, jwt_secret_bytes) = tokio::select! {
            _ = register::register(tx, kill_rx, our_ip.clone(), http_server_port, http_server_port)
                => panic!("registration failed"),
            (our, password, serialized_networking_keypair, jwt_secret_bytes) = async {
                while let Some(fin) = rx.recv().await {
                    return fin
                }
                panic!("registration failed")
            } => (our, password, serialized_networking_keypair, jwt_secret_bytes),
        };

        println!("generating disk encryption keys...");
        let mut disk_key: DiskKey = [0u8; CREDENTIAL_LEN];
        pbkdf2::derive(
            PBKDF2_ALG,
            NonZeroU32::new(ITERATIONS).unwrap(),
            DISK_KEY_SALT,
            password.as_bytes(),
            &mut disk_key,
        );
        println!(
            "saving encrypted networking keys to {}/.network.keys",
            home_directory_path
        );
        let key = Key::<Aes256Gcm>::from_slice(&disk_key);
        let cipher = Aes256Gcm::new(&key);
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng); // 96-bits; unique per message
        let keyciphertext: Vec<u8> = cipher
            .encrypt(&nonce, serialized_networking_keypair.as_ref())
            .unwrap();

        let jwtciphertext: Vec<u8> = cipher.encrypt(&nonce, jwt_secret_bytes.as_ref()).unwrap();

        fs::write(
            format!("{}/.network.keys", home_directory_path),
            bincode::serialize(&(
                our.name.clone(),
                our.allowed_routers.clone(),
                [nonce.deref().to_vec(), keyciphertext].concat(),
                [nonce.deref().to_vec(), jwtciphertext].concat(),
            ))
            .unwrap(),
        )
        .await
        .unwrap();

        let networking_keypair =
            signature::Ed25519KeyPair::from_pkcs8(serialized_networking_keypair.as_ref()).unwrap();

        println!("registration complete!");
        (our, networking_keypair, jwt_secret_bytes.to_vec())
    };
    //  bootstrap FS.
    let _ = print_sender
        .send(Printout {
            verbosity: 0,
            content: "bootstrapping fs...".to_string(),
        })
        .await;

    let (kernel_process_map, manifest) =
        filesystem::bootstrap(our.name.clone(), home_directory_path.clone())
            .await
            .expect("fs bootstrap failed!");

    let _ = kill_tx.send(true);
    let _ = print_sender
        .send(Printout {
            verbosity: 0,
            content: format!("{} now online", our.name),
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
        networking_keypair_arc.clone(),
        home_directory_path.into(),
        kernel_process_map.clone(),
        caps_oracle_sender.clone(),
        caps_oracle_receiver,
        kernel_message_sender.clone(),
        print_sender.clone(),
        kernel_message_receiver,
        network_error_receiver,
        kernel_debug_message_receiver,
        net_message_sender.clone(),
        fs_message_sender,
        http_server_sender,
        http_client_sender,
        eth_rpc_sender,
        vfs_message_sender,
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
        manifest,
        kernel_message_sender.clone(),
        print_sender.clone(),
        fs_message_receiver,
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
    tasks.spawn(vfs::vfs(
        our.name.clone(),
        kernel_process_map,
        kernel_message_sender.clone(),
        print_sender.clone(),
        vfs_message_receiver,
        caps_oracle_sender.clone(),
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
            signed_capabilities: None,
        })
        .await;
    // abort all remaining tasks
    tasks.shutdown().await;
    let _ = crossterm::terminal::disable_raw_mode();
    println!("");
    println!("\x1b[38;5;196m{}\x1b[0m", quit_msg);
    return;
}
