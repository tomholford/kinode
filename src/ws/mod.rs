use crate::types::*;
use crate::ws::connections::*;
use aes_gcm_siv::Nonce;
use elliptic_curve::ecdh::EphemeralSecret;
use elliptic_curve::PublicKey;
use ethers::prelude::k256::Secp256k1;
use ethers::prelude::*;
use futures::prelude::*;
use futures::stream::{SplitSink, SplitStream};
use ring::signature::{self, Ed25519KeyPair};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::{self};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

mod connections;
mod listener;
mod sender;

type Peers = Arc<RwLock<HashMap<String, Peer>>>;
type Routers = Arc<RwLock<HashMap<String, Router>>>;
type Sock = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WriteStream = SplitSink<Sock, tungstenite::Message>;
// route-to,       outside-source -- stream can be from EITHER
type PassThroughs = Arc<RwLock<HashMap<String, HashMap<String, WriteStream>>>>;

pub struct Peer {
    pub networking_address: H256,
    pub ephemeral_secret: Arc<EphemeralSecret<Secp256k1>>,
    pub their_ephemeral_pk: Arc<PublicKey<Secp256k1>>,
    pub nonce: Arc<Nonce>,
    // must have exactly one of these two
    pub router: Option<String>,
    pub direct_write_stream: Option<WriteStream>,
}

pub struct Router {
    name: String,
    pending_peers: HashMap<String, (Arc<EphemeralSecret<Secp256k1>>, Vec<u8>, Message)>,
}

/// contains identity and encryption keys, used in initial handshake.
/// parsed from Text websocket message
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Handshake {
    from: String,
    target: String,
    routing_request: bool,
    id_signature: Vec<u8>,
    ephemeral_public_key: Vec<u8>,
    ephemeral_public_key_signature: Vec<u8>,
    nonce: Option<Vec<u8>>,
}

enum SuccessOrTimeout {
    Success,
    Timeout,
    TryAgain,
}

/// parsed from Binary websocket message
#[derive(Debug, Serialize, Deserialize)]
enum WrappedMessage {
    From(RoutedFrom),
    To(RouteTo),
    Handshake(Handshake),
    LostPeer(String),
    PeerOffline(String),
}

#[derive(Debug, Serialize, Deserialize)]
struct RouteTo {
    pub to: String,
    pub contents: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RoutedFrom {
    pub from: String,
    pub contents: Vec<u8>,
}

/// websockets driver.
///
/// statelessly manages websocket connections to peer nodes
pub async fn websockets(
    our: Identity,
    keypair: Ed25519KeyPair,
    pki: OnchainPKI,
    message_tx: MessageSender,
    print_tx: PrintSender,
    message_rx: MessageReceiver,
    self_message_tx: MessageSender,
) {
    // initialize peer-connection-map as empty -- can pre-populate as optimization?
    let peers: Peers = Arc::new(RwLock::new(HashMap::new()));

    let keypair = Arc::new(keypair);

    // if we have networking info published in the pki,
    // open both a sender and a listener.
    // otherwise, only open a sender.
    match our.ws_routing {
        None => {
            tokio::join!(sender::ws_sender(
                our.clone(),
                keypair.clone(),
                pki.clone(),
                peers.clone(),
                message_tx.clone(),
                print_tx.clone(),
                message_rx,
                self_message_tx.clone(),
            ));
        }
        Some((_, port)) => {
            let tcp_listener = TcpListener::bind(format!("0.0.0.0:{}", port))
                .await
                .expect(format!("fatal error: can't listen on port {}", port).as_str());

            let _ = print_tx
                .send(format!("now listening on port {}", port))
                .await;

            // listen on our port for new connections, and
            // listen on our receiver for messages to send to peers
            tokio::join!(
                listener::ws_listener(
                    our.clone(),
                    keypair.clone(),
                    pki.clone(),
                    peers.clone(),
                    message_tx.clone(),
                    print_tx.clone(),
                    tcp_listener,
                ),
                sender::ws_sender(
                    our.clone(),
                    keypair.clone(),
                    pki.clone(),
                    peers.clone(),
                    message_tx.clone(),
                    print_tx.clone(),
                    message_rx,
                    self_message_tx.clone(),
                ),
            );
        }
    }
}

/*
 *  handshake utils
 */

/// read one message from websocket stream and parse it as a handshake.
async fn get_handshake(ws_stream: &mut SplitStream<Sock>) -> Result<Handshake, String> {
    let handshake_text = ws_stream
        .next()
        .await
        .ok_or("handshake failed")?
        .map_err(|e| format!("{}", e))?
        .into_text()
        .map_err(|e| format!("{}", e))?;
    let handshake: Handshake =
        serde_json::from_str(&handshake_text).map_err(|_| "got bad handshake")?;
    Ok(handshake)
}

/// take in handshake and PKI identity, and confirm that the handshake is valid.
/// takes in optional nonce, which must be the one that connection initiator created.
fn validate_handshake(
    handshake: &Handshake,
    their_id: &Identity,
    nonce: Option<Vec<u8>>,
) -> Result<(Arc<PublicKey<Secp256k1>>, Arc<Nonce>), String> {
    let their_networking_key = signature::UnparsedPublicKey::new(
        &signature::ED25519,
        hex::decode(&their_id.networking_key).map_err(|_| "failed to decode networking key")?,
    );

    if !(their_networking_key
        .verify(
            &serde_json::to_vec(&their_id).map_err(|_| "failed to serialize their identity")?,
            &handshake.id_signature,
        )
        .is_ok()
        && their_networking_key
            .verify(
                &handshake.ephemeral_public_key,
                &handshake.ephemeral_public_key_signature,
            )
            .is_ok())
    {
        // improper signatures on identity info, close connection
        return Err("got improperly signed networking info".into());
    }

    let their_ephemeral_pk =
        match PublicKey::<Secp256k1>::from_sec1_bytes(&handshake.ephemeral_public_key) {
            Ok(v) => Arc::new(v),
            Err(_) => return Err("error".into()),
        };

    // assign nonce based on our role in the connection
    let nonce: Arc<Nonce> = match nonce {
        Some(n) => Arc::new(*Nonce::from_slice(&n)),
        None => Arc::new(*Nonce::from_slice(
            &handshake.nonce.as_ref().ok_or("got bad nonce")?,
        )),
    };

    return Ok((their_ephemeral_pk, nonce));
}

/// given an identity and networking key-pair, produces a handshake message along
/// with an ephemeral secret to be used in a specific connection.
fn make_secret_and_handshake(
    our: &Identity,
    keypair: Arc<Ed25519KeyPair>,
    target: String,
    nonce: Option<Arc<Nonce>>,
    is_routing_request: bool,
) -> Result<(Arc<EphemeralSecret<Secp256k1>>, Handshake), String> {
    // produce ephemeral keys for DH exchange and subsequent symmetric encryption
    let ephemeral_secret = Arc::new(EphemeralSecret::<k256::Secp256k1>::random(
        &mut rand::rngs::OsRng,
    ));
    let ephemeral_public_key = ephemeral_secret.public_key();
    // sign the ephemeral public key with our networking management key
    let signed_pk = keypair
        .sign(&ephemeral_public_key.to_sec1_bytes())
        .as_ref()
        .to_vec();
    let signed_id = keypair
        .sign(&serde_json::to_vec(our).map_err(|e| format!("{}", e))?)
        .as_ref()
        .to_vec();

    let nonce = match nonce {
        Some(_) => None,
        None => {
            let mut iv = [0u8; 12];
            rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut iv);
            Some(iv.to_vec())
        }
    };

    let handshake = Handshake {
        from: our.name.clone(),
        target: target.clone(),
        routing_request: is_routing_request,
        id_signature: signed_id,
        ephemeral_public_key: ephemeral_public_key.to_sec1_bytes().to_vec(),
        ephemeral_public_key_signature: signed_pk,
        // if we are connection initiator, send nonce inside message
        nonce: nonce.clone(),
    };

    Ok((ephemeral_secret, handshake))
}
