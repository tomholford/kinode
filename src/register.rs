use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm,
    Key, // Or `Aes128Gcm`
};
use ring::pbkdf2;
use ring::pkcs8::Document;
use ring::rand::SystemRandom;
use ring::signature::{self, KeyPair};
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use warp::{Filter, Rejection, Reply, http::{header::{HeaderValue, SET_COOKIE}}};
use jwt::SignWithKey;
use hmac::Hmac;
use sha2::Sha256;
use serde::{Serialize, Deserialize};
use std::collections::HashMap; // TODO delete

use crate::types::*;

type RegistrationSender = mpsc::Sender<(Registration, Document, Vec<u8>, String)>;

static PBKDF2_ALG: pbkdf2::Algorithm = pbkdf2::PBKDF2_HMAC_SHA256; // TODO maybe look into Argon2
pub const ITERATIONS: u32 = 1_000_000;
pub const DISK_KEY_SALT: &'static [u8; 32] = b"742d35Cc6634C0532925a3b844Bc454e";

pub fn generate_jwt(jwt_secret_bytes: &[u8], username: String) -> Option<String> {
    let jwt_secret: Hmac<Sha256> = match Hmac::new_from_slice(&jwt_secret_bytes) {
        Ok(secret) => secret,
        Err(_) => return None,
    };

    let claims = JwtClaims {
        username: username.clone(),
        expiration: 0,
    };

    match claims.sign_with_key(&jwt_secret) {
        Ok(token) => Some(token),
        Err(_) => None,
    }
}

/// Serve the registration page and receive POSTs and PUTs from it
pub async fn register(
    tx: RegistrationSender,
    kill_rx: oneshot::Receiver<bool>,
    port: u16,
    redir_port: u16,
) {
    const REGISTER_PAGE: &str = include_str!("register.html");

    let registration = Arc::new(Mutex::new(None));
    let networking_keypair = Arc::new(Mutex::new(None));
    let jwt_secret = Arc::new(Mutex::new(None));

    let registration_post = registration.clone();
    let networking_keypair_post = networking_keypair.clone();
    let jwt_secret_post = jwt_secret.clone();

    let check_address_route = warp::path!("check-address" / String).and_then({
        // TODO need to get rid of this
        let pki: OnchainPKI = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        move |address: String| {
            let pki = pki.clone();
            async move {
                let pki_read = pki.read().await;

                let mut usernames: Vec<String> = Vec::new();

                for (username, identity) in pki_read.iter() {
                    if identity.address == address {
                        usernames.push(username.to_string());
                    }
                }

                if !usernames.is_empty() {
                    Ok::<_, Rejection>(warp::reply::with_status(
                        usernames.join(","),
                        warp::http::StatusCode::OK,
                    ))
                } else {
                    Ok::<_, Rejection>(warp::reply::with_status(
                        "Not taken".to_string(),
                        warp::http::StatusCode::NO_CONTENT,
                    ))
                }
            }
        }
    });

    let check_username_route = warp::path!("check-username" / String).and_then({
        // TODO need to get rid of this
        let pki: OnchainPKI = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        move |username: String| {
            let pki = pki.clone();
            async move {
                let pki_read = pki.read().await;

                let reply = match pki_read.get(&username) {
                    Some(_) => warp::reply::with_status(
                        "Conflict".to_string(),
                        warp::http::StatusCode::CONFLICT,
                    ),
                    None => warp::reply::with_status(
                        "Not taken".to_string(),
                        warp::http::StatusCode::NO_CONTENT,
                    ),
                };
                Ok::<_, Rejection>(reply)
            }
        }
    });

    let routes = warp::path("register").and(
        // 1. serve register.html right here
        warp::get()
            .map(move || warp::reply::html(REGISTER_PAGE))
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

                    // Generate the jwt_secret
                    let mut jwt_secret = [0u8; 32];
                    ring::rand::SecureRandom::fill(&seed, &mut jwt_secret).unwrap();
                    *jwt_secret_post.lock().unwrap() = Some(jwt_secret);

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
                .and(warp::any().map(move || jwt_secret.lock().unwrap().take().unwrap()))
                .and(warp::any().map(move || redir_port))
                .and_then(handle_put)),
    ).or(check_address_route).or(check_username_route);

    let _ = open::that(format!("http://localhost:{}/register", port));
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
    jwt_secret_bytes: [u8; 32],
    _redir_port: u16,
) -> Result<impl Reply, Rejection> {
    let token = match generate_jwt(&jwt_secret_bytes, registration.username.clone()) {
        Some(token) => token,
        None => return Err(warp::reject()),
    };
    let cookie_value = format!("uqbar-auth_{}={};", &registration.username, &token);
    let ws_cookie_value = format!("uqbar-ws-auth_{}={};", &registration.username, &token);

    let mut response = warp::reply::html("Success".to_string()).into_response();
            
    let headers = response.headers_mut();
    headers.append(SET_COOKIE, HeaderValue::from_str(&cookie_value).unwrap());
    headers.append(SET_COOKIE, HeaderValue::from_str(&ws_cookie_value).unwrap());

    sender
        .send((registration, networking_keypair, jwt_secret_bytes.to_vec(), signature))
        .await
        .unwrap();
    Ok(response)
}

/// Serve the login page, just get a password
pub async fn login(
    tx: mpsc::Sender<(signature::Ed25519KeyPair, Vec<u8>)>,
    kill_rx: oneshot::Receiver<bool>,
    keyfile: Vec<u8>,
    jwt_secret_file: Vec<u8>,
    port: u16,
    username: &str,
) {
    let username = username.to_string();
    let login_page_content = include_str!("login.html");
    let personalized_login_page = login_page_content.replace("${our}", username.as_str());
    let redirect_to_login = warp::path::end().map(|| warp::redirect(warp::http::Uri::from_static("/login")));
    let routes = warp::path("login").and(
        // 1. serve login.html right here
        warp::get()
            .map(move || warp::reply::html(personalized_login_page.clone()))
            // 2. await a single POST
            //    - password
            .or(warp::post()
                .and(warp::body::content_length_limit(1024 * 16))
                .and(warp::body::json())
                .and(warp::any().map(move || keyfile.clone()))
                .and(warp::any().map(move || jwt_secret_file.clone()))
                .and(warp::any().map(move || username.clone()))
                .and(warp::any().map(move || tx.clone()))
                .and_then(handle_password)),
    ).or(redirect_to_login);

    let _ = open::that(format!("http://localhost:{}/login", port));
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
    jwt_secret_file: Vec<u8>,
    username: String,
    tx: mpsc::Sender<(signature::Ed25519KeyPair, Vec<u8>)>,
) -> Result<impl Reply, Rejection> {
    let password = match password["password"].as_str() {
        Some(p) => p,
        None => return Err(warp::reject()),
    };
    // use password to decrypt networking keys
    println!("decrypting saved networking key...");
    let nonce = digest::generic_array::GenericArray::from_slice(&keyfile[..12]);

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
            println!("failed to decrypt networking keys: {}", e);
            return Err(warp::reject());
        }
    };
    let networking_keypair = match signature::Ed25519KeyPair::from_pkcs8(&pkcs8_string) {
        Ok(k) => k,
        Err(_) => return Err(warp::reject()),
    };

    // TODO: check if jwt_secret_file is valid and then proceed to unwrap and decrypt. If there is a failure, generate a new jwt_secret and save it
    // use password to decrypt jwt secret
    println!("decrypting saved jwt secret...");
    let jwt_nonce = digest::generic_array::GenericArray::from_slice(&jwt_secret_file[..12]);

    let jwt_secret_bytes: Vec<u8> = match cipher.decrypt(jwt_nonce, &jwt_secret_file[12..]) {
        Ok(p) => p,
        Err(e) => {
            println!("failed to decrypt jwt secret: {}", e);
            return Err(warp::reject());
        }
    };

    let token = match generate_jwt(&jwt_secret_bytes, username.clone()) {
        Some(token) => token,
        None => return Err(warp::reject()),
    };
    let cookie_value = format!("uqbar-auth_{}={};", &username, &token);
    let ws_cookie_value = format!("uqbar-ws-auth_{}={};", &username, &token);

    let mut response = warp::reply::html("Success".to_string()).into_response();
            
    let headers = response.headers_mut();
    headers.append(SET_COOKIE, HeaderValue::from_str(&cookie_value).unwrap());
    headers.append(SET_COOKIE, HeaderValue::from_str(&ws_cookie_value).unwrap());
    
    tx.send((networking_keypair, jwt_secret_bytes)).await.unwrap();
    // TODO unhappy paths where key has changed / can't be decrypted
    Ok(response)
}
