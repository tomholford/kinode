use std::num::NonZeroU32;
use serde::{Serialize, Deserialize};
use crate::types::*;
use serde_json::json;
use ring::{digest, pbkdf2};
use ring::signature;
use ring::rand as RingRand;

static PBKDF2_ALG: pbkdf2::Algorithm = pbkdf2::PBKDF2_HMAC_SHA256; // TODO maybe look into Argon2

const CREDENTIAL_LEN: usize = digest::SHA256_OUTPUT_LEN;
const ITERATIONS: u32 = 5_000_000; // took 8 seconds on my machine

pub type Credential = [u8; CREDENTIAL_LEN];

#[derive(Debug, Serialize, Deserialize)]
enum KeygenAction {
  GenerateDiskKey(GenerateDiskKey),
  GenerateNetworkingKey,
  // TODO this is just the format of the return from fs...we should probably change that format
  Write(String),
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
  while let Some(message) = recv_in_kg.recv().await {
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
  wm: WrappedMessage,
  print_tx: PrintSender,
) {
  let Some(value) = wm.message.payload.json.clone() else {
    panic!("keygen: request must have JSON payload, got: {:?}", wm.message);
  };
  let act: KeygenAction = serde_json::from_value(value).unwrap();

  match act {
    KeygenAction::GenerateDiskKey(action) => {
      if wm.message.wire.source_ship != our {
        panic!("keygen: source_ship must be our ship, got: {:?}", wm.message.wire.source_ship)
      };
      if action.salt.as_bytes().len() != 32 {
        panic!("salt must be 32 bytes long")
      };

      // TODO verify that sys.keys doesn't exist in fs, functionality doesn't exist yet

      let _ = print_tx.send(Printout {
        verbosity: 0,
        content: format!("generating sys.keys...")
      }).await;
      let mut to_store: Credential = [0u8; CREDENTIAL_LEN];      
      pbkdf2::derive(PBKDF2_ALG, NonZeroU32::new(ITERATIONS).unwrap(), action.salt.as_bytes(), action.password.as_bytes(), &mut to_store);
      // store them in fs
      //
      let _ = send_to_loop.send(
        WrappedMessage {
          id: rand::random(),
          rsvp: None, // TODO what is this
          message: Message {
            message_type: MessageType::Request(true),
            wire: Wire {
              source_ship: our.clone(),
              source_app: "keygen".to_string(),
              target_ship: our.clone(),
              target_app: "filesystem".to_string(),
            },
            payload: Payload {
              json: Some(json!({"uri_string": "fs://disk.keys", "action": "Write"})),
              bytes: Some(to_store.to_vec())
            }
          }
        }
      ).await;
    },
    KeygenAction::GenerateNetworkingKey => {
      let _ = print_tx.send(Printout {
        verbosity : 0,
        content: format!("generating network.keys...")
      }).await;
      // TODO uncomment this once boot sequence is real
      // if message.wire.source_app != "ws" {
      //   panic!("keygen: source_app must be ws, got: {:?}", message.wire.source_app)
      // };
      let seed = RingRand::SystemRandom::new();
      let networking_keypair = signature::Ed25519KeyPair::generate_pkcs8(&seed).unwrap();

      // send key as a response to the networking module
      let _ = send_to_loop.send(
        WrappedMessage {
          id: wm.id,
          rsvp: None,
          message: Message {
            message_type: MessageType::Response,
            wire: match wm.message.message_type {
              MessageType::Response => panic!("keygen: should not get a response for GenerateNetworkingKey"),
              MessageType::Request(is_expecting_response) => {
                if is_expecting_response {
                  Wire {
                    source_ship: our.clone(),
                    source_app: "keygen".to_string(),
                    target_ship: our.clone(),
                    target_app: wm.message.wire.source_app.clone(),
                  }
                } else {
                  let Some(rsvp) = wm.rsvp else { panic!("keygen: must have rsvp for GenerateNetworkingKey"); };
                  Wire {
                    source_ship: our.clone(),
                    source_app: "keygen".to_string(),
                    target_ship: rsvp.node.clone(),
                    target_app: rsvp.process.clone(),
                  }
                }
              },
            },
            payload: Payload {
              json: None,
              bytes: Some(networking_keypair.as_ref().to_vec())
            }
          }
        }
      ).await;
    },
    // handles response from fs
    KeygenAction::Write(file_path) => {
      // TODO add error propagation from fs once that is built
      let _ = print_tx.send(Printout {
        verbosity: 0,
        content: format!("successfully saved keys to {:?}", file_path)
      }).await;
    }
  }
}
