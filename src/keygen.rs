use std::num::NonZeroU32;
use serde::{Serialize, Deserialize};
use crate::types::*;
use serde_json::json;
use ring::{digest, pbkdf2};
use ring::signature;
use ring::signature::KeyPair;

static PBKDF2_ALG: pbkdf2::Algorithm = pbkdf2::PBKDF2_HMAC_SHA256; // maybe switch to Argon2

const CREDENTIAL_LEN: usize = digest::SHA256_OUTPUT_LEN;
const ITERATIONS: u32 = 5_000_000; // took 8 seconds on my machine

pub type Credential = [u8; CREDENTIAL_LEN];

// keygen test commands
// !message tuna keygen {"GenerateDiskKey":{"password":"pw","salt":"742d35Cc6634C0532925a3b844Bc454e"}}
// !message tuna keygen "GenerateNetworkingKey"

#[derive(Debug, Serialize, Deserialize)]
enum KeygenAction {
  // TODO this is just the format of the return from fs
  //      we should probably change that format
  uri_string(String),
  GenerateDiskKey(GenerateDiskKey),
  GenerateNetworkingKey,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GenerateDiskKey {
  password: String,
  salt: String,
}

pub async fn keygen(
  our_name: &str,
  send_to_loop: MessageSender,
  mut recv_in_kg: MessageReceiver,
  print_tx: PrintSender,
) {
  while let Some(mut messages) = recv_in_kg.recv().await {
    let Some(message) = messages.pop() else {
      panic!("keygen: keygen must receive non-empty MessageStack, got: {:?}", messages);
    };

    tokio::spawn(
      handle_request(
        our_name.to_string(),
        send_to_loop.clone(),
        message,
        print_tx.clone()
      )
    );
  }
}

async fn handle_request(
  our: String,
  send_to_loop: MessageSender,
  message: Message,
  print_tx: PrintSender,
) {
  let Some(value) = message.payload.json.clone() else {
    panic!("keygen: request must have JSON payload, got: {:?}", message);
  };
  let act: KeygenAction = serde_json::from_value(value).unwrap();

  match act {
    KeygenAction::GenerateDiskKey(action) => {
      if message.wire.source_ship != our {
        panic!("keygen: source_ship must be our ship, got: {:?}", message.wire.source_ship)
      };
      if action.salt.as_bytes().len() != 32 {
        panic!("salt must be 32 bytes long")
      };

      // TODO verify that sys.keys doesn't exist in fs

      let _ = print_tx.send(format!("generating sys.keys...")).await;
      let mut to_store: Credential = [0u8; CREDENTIAL_LEN];      
      pbkdf2::derive(PBKDF2_ALG, NonZeroU32::new(ITERATIONS).unwrap(), action.salt.as_bytes(), action.password.as_bytes(), &mut to_store);
      // store them in fs
      //
      let _ = send_to_loop.send(vec![
        Message {
          message_type: MessageType::Request(true), // TODO maybe this should be a Response that someone should call, API style
          wire: Wire {
            source_ship: our.clone(),
            source_app: "keygen".to_string(),
            target_ship: our.clone(),
            target_app: "filesystem".to_string(),
          },
          payload: Payload {
            json: Some(json!({"Write": "fs://disk.keys"})),
            bytes: Some(to_store.to_vec())
          }
        }
      ]).await;
    },
    KeygenAction::GenerateNetworkingKey => {
      let _ = print_tx.send(format!("generating network.keys...")).await;
      // TODO this is the real random code but for now just use a fixed value
      // let mut rng = StdRng::from_entropy();
      // let mut seed = [0u8; 32];
      // rng.fill(&mut seed);
      // TODO this is the fixed networking key code
      let bytes: &[u8] = our.as_bytes();
      let mut seed: [u8; 32] = [0; 32];
      let copy_len = bytes.len().min(32);
      seed[..copy_len].copy_from_slice(&bytes[..copy_len]);
      // END

      let networking_keypair = signature::Ed25519KeyPair::from_seed_unchecked(&seed).unwrap();
      let hex_pubkey = hex::encode(networking_keypair.public_key().as_ref());
      let _ = print_tx.send(format!("our networking key: {:?}", hex_pubkey)).await;
      // send key as a response to the networking module
      let _ = send_to_loop.send(vec![
        Message {
          message_type: MessageType::Response,
          wire: Wire {
            source_ship: our.clone(),
            source_app: "keygen".to_string(),
            target_ship: our.clone(),
            target_app: message.wire.source_app.clone(),
          },
          payload: Payload {
            json: None,// TODO Some(json!({"Write": "fs://disk.keys"})),
            bytes: None
          }
        }
      ]).await;
    },
    // handles response from fs
    _ => {
      // TODO error propagation from fs when that is built
      let _ = print_tx.send("successfully saved keys to fs://sys.keys".to_string()).await;
    }
  }
}
