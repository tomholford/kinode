use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm,
    Key, // Or `Aes128Gcm`
};
use http::Uri;
use ring::pbkdf2;
use ring::pkcs8::Document;
use ring::rand::SystemRandom;
use ring::signature::{self, KeyPair};
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use warp::{redirect, Filter};

use crate::types::*;

type RegistrationSender = mpsc::Sender<(Registration, Document, String)>;

static PBKDF2_ALG: pbkdf2::Algorithm = pbkdf2::PBKDF2_HMAC_SHA256; // TODO maybe look into Argon2
pub const ITERATIONS: u32 = 1_000_000;
pub const DISK_KEY_SALT: &'static [u8; 32] = b"742d35Cc6634C0532925a3b844Bc454e";

/// Serve the registration page and receive POSTs and PUTs from it
pub async fn register(tx: RegistrationSender, kill_rx: oneshot::Receiver<bool>, port: u16) {
    const REGISTER_PAGE: &str = include_str!("register.html");

    let registration = Arc::new(Mutex::new(None));
    let networking_keypair = Arc::new(Mutex::new(None));

    let registration_post = registration.clone();
    let networking_keypair_post = networking_keypair.clone();

    let routes = warp::path("register").and(
        // 1. serve register.html right here
        warp::get()
            .map(move || warp::reply::html(REGISTER_PAGE.clone()))
            // 2. await a single POST
            //    - username
            //    - password
            //    - address (wallet)
            .or(warp::post()
                .and(warp::body::content_length_limit(1024 * 16))
                .and(warp::body::json())
                .map(move |info: Registration| {
                    // Process the data from the POST request here and store it
                    *registration_post.lock().unwrap() = Some(info);

                    // this will be replaced with the key manager module
                    let seed = SystemRandom::new();
                    let serialized_keypair =
                        signature::Ed25519KeyPair::generate_pkcs8(&seed).unwrap();
                    let keypair =
                        signature::Ed25519KeyPair::from_pkcs8(serialized_keypair.as_ref()).unwrap();

                    let public_key = hex::encode(keypair.public_key().as_ref());
                    *networking_keypair_post.lock().unwrap() = Some(serialized_keypair);
                    // Return a response to the POST request containing new networking key to be signed
                    warp::reply::html(public_key)
                }))
            // 4. await a PUT
            //    - signature string
            .or(warp::put()
                .and(warp::body::content_length_limit(1024 * 16))
                .and(warp::body::json())
                .and(warp::any().map(move || tx.clone()))
                .and(warp::any().map(move || registration.lock().unwrap().take().unwrap()))
                .and(warp::any().map(move || networking_keypair.lock().unwrap().take().unwrap()))
                .and_then(handle_put)),
    );

    open::that(format!("http://localhost:{}/register", port)).unwrap();
    warp::serve(routes)
        .bind_with_graceful_shutdown(([0, 0, 0, 0], port), async {
            kill_rx.await.ok();
        })
        .1
        .await;
}

async fn handle_put(
    signature: String,
    sender: RegistrationSender,
    registration: Registration,
    networking_keypair: Document,
) -> Result<impl warp::Reply, warp::Rejection> {
    sender
        .send((registration, networking_keypair, signature))
        .await
        .unwrap();
    // TODO make this redirect work correctly
    Ok(redirect::found(format!("http://localhost:{}", 8080).parse::<Uri>().unwrap()))
}

/// Serve the login page, just get a password
pub async fn login(
    tx: mpsc::Sender<signature::Ed25519KeyPair>,
    kill_rx: oneshot::Receiver<bool>,
    keyfile: Vec<u8>,
    port: u16,
    username: &str,
) {
    let login_page_content = include_str!("login.html");
    let personalized_login_page = login_page_content.replace("${our}", &username);
    let routes = warp::path("login").and(
        // 1. serve register.html right here
        warp::get()
            .map(move || warp::reply::html(personalized_login_page.clone()))
            // 2. await a single POST
            //    - password
            .or(warp::post()
                .and(warp::body::content_length_limit(1024 * 16))
                .and(warp::body::json())
                .and(warp::any().map(move || keyfile.clone()))
                .and(warp::any().map(move || tx.clone()))
                .and_then(handle_password)),
    );

    open::that(format!("http://localhost:{}/login", port)).unwrap();
    warp::serve(routes)
        .bind_with_graceful_shutdown(([0, 0, 0, 0], port), async {
            kill_rx.await.ok();
        })
        .1
        .await;
}

async fn handle_password(
    password: serde_json::Value,
    keyfile: Vec<u8>,
    tx: mpsc::Sender<signature::Ed25519KeyPair>,
) -> Result<impl warp::Reply, warp::Rejection> {
    let password = match password["password"].as_str() {
        Some(p) => p,
        None => return Err(warp::reject()),
    };
    // use password to decrypt networking keys
    let nonce = digest::generic_array::GenericArray::from_slice(&keyfile[..12]);

    println!("decrypting saved networking key...");
    let mut disk_key: DiskKey = [0u8; CREDENTIAL_LEN];
    pbkdf2::derive(
        PBKDF2_ALG,
        NonZeroU32::new(ITERATIONS).unwrap(),
        DISK_KEY_SALT,
        password.as_bytes(),
        &mut disk_key,
    );
    let key = Key::<Aes256Gcm>::from_slice(&disk_key);
    let cipher = Aes256Gcm::new(&key);
    let pkcs8_string: Vec<u8> = match cipher.decrypt(nonce, &keyfile[12..]) {
        Ok(p) => p,
        Err(e) => {
            println!("failed to decrypt: {}", e);
            return Err(warp::reject());
        }
    };
    let networking_keypair = match signature::Ed25519KeyPair::from_pkcs8(&pkcs8_string) {
        Ok(k) => k,
        Err(_) => return Err(warp::reject()),
    };
    tx.send(networking_keypair).await.unwrap();
    // TODO unhappy paths where key has changed / can't be decrypted
    // TODO make this redirect work correctly
    Ok(redirect::found(format!("http://localhost:{}", 8080).parse::<Uri>().unwrap()))
}
