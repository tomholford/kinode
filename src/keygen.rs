use crate::types::*;
use serde::{Serialize, Deserialize};
use serde_json::json;
use ring::{digest, pbkdf2};
use std::{collections::HashMap, num::NonZeroU32};


static PBKDF2_ALG: pbkdf2::Algorithm = pbkdf2::PBKDF2_HMAC_SHA256; // maybe switch to Argon2

const CREDENTIAL_LEN: usize = digest::SHA256_OUTPUT_LEN;
const ITERATIONS: u32 = 5_000_000; // took 8 seconds on my machine

pub type Credential = [u8; CREDENTIAL_LEN];

// !message tuna keygen {"GenerateKey":{"password":"pw","salt":"742d35Cc6634C0532925a3b844Bc454e"}}

#[derive(Debug, Serialize, Deserialize)]
enum KeygenAction {
  uri_string(String),
  GenerateKey(GenerateKey),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GenerateKey {
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
    let Some(mut message) = messages.pop() else {
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
    KeygenAction::GenerateKey(action) => {
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
          message_type: MessageType::Request(true),
          wire: Wire {
            source_ship: our.clone(),
            source_app: "keygen".to_string(),
            target_ship: our.clone(),
            target_app: "filesystem".to_string(),
          },
          payload: Payload {
            json: Some(json!({"Write": "fs://sys.keys"})),
            bytes: Some(to_store.to_vec())
          }
        }
      ]).await;
    },
    // handles response from fs
    _ => {
      // TODO error propagation from fs when that is built
      let _ = print_tx.send(format!("successfully saved keys to fs://sys.keys", act)).await;
    }
  }
}