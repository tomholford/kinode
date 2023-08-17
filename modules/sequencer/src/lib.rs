cargo_component_bindings::generate!();

use cita_trie::MemoryDB;
use cita_trie::PatriciaTrie;
use hasher::{Hasher, HasherKeccak};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;

use bindings::component::microkernel_process::types;

struct Component;

#[derive(Debug, Serialize, Deserialize)]
pub struct FileSystemRequest {
    pub uri_string: String,
    pub action: FileSystemAction,
}
#[derive(Debug, Serialize, Deserialize)]
pub enum FileSystemAction {
    Read,
    Write,
    GetMetadata,
    OpenRead,
    OpenAppend,
    Append,
    ReadChunkFromOpen(u64),
    SeekWithinOpen(FileSystemSeekFrom),
}
#[derive(Debug, Serialize, Deserialize)]
pub enum FileSystemSeekFrom {
    Start(u64),
    End(i64),
    Current(i64),
}

//
// chain types
//

// #[derive(Debug, Serialize, Deserialize)]
// struct SequencerState {
//     pub last_batch: u64,
//     pub last_batch_hash: String,
//     pub mempool: Vec<Transaction>,
// }

// #[derive(Debug, Serialize, Deserialize)]
// enum SequencerMessage {
//     MakeBatch,
//     Transaction(Transaction),
// }

// #[derive(Debug, Clone, Serialize, Deserialize)]
// enum Item {
//     State(StateItem),
//     Contract(ContractItem),
// }

// #[derive(Debug, Clone, Serialize, Deserialize)]
// struct StateItem {
//     pub source: String,
//     pub holder: String,
//     pub town_id: u32,
//     pub salt: u32,
//     pub label: String,
//     pub data: serde_json::Value,
// }

// #[derive(Debug, Clone, Serialize, Deserialize)]
// struct ContractItem {
//     pub source: String,
//     pub holder: String,
//     pub town_id: u32,
//     pub code_hex: String, // source code of contract represented as hex string
// }

// struct UqChain {
//     state: PatriciaTrie<MemoryDB, HasherKeccak>,
//     nonces: HashMap<String, String>, // map of address to current nonce
// }

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IdentityTransaction {
    pub from: String,
    pub signature: Option<String>,
    pub to: String, // contract address
    pub town_id: u32,
    pub calldata: Identity,
    pub nonce: String,
}

type OnchainPKI = HashMap<String, Identity>;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Identity {
    pub name: String,
    pub address: String,
    pub networking_key: String,
    pub ws_routing: Option<(String, u16)>,
    pub allowed_routers: Vec<String>,
}

//
// app logic
//

fn parse_transaction(tx: IdentityTransaction) -> Option<Identity> {
    // TODO
    if tx.from != tx.calldata.address {
        return None;
    }
    return Some(tx.calldata);
}

fn failure(http_head: serde_json::Value) {
    bindings::send_response(Ok((
        &types::WitPayload {
            json: Some(
                serde_json::json!({
                    "action": "response",
                    "id": http_head["id"],
                    "status": 400,
                    "headers": {
                        "Content-Type": "application/json",
                    },
                })
                .to_string(),
            ),
            bytes: Some("bad transaction".as_bytes().to_vec()),
        },
        "",
    )));
}

impl bindings::MicrokernelProcess for Component {
    fn run_process(our_name: String, process_name: String) {
        assert!(process_name == "sequencer");
        bindings::print_to_terminal(1, "sequencer: initialized");

        // get blockchain.json from home directory
        bindings::send_requests(Ok((
            vec![
                types::WitProtorequest {
                    is_expecting_response: true,
                    target: types::WitProcessNode {
                        node: our_name.clone(),
                        process: "filesystem".into(),
                    },
                    payload: types::WitPayload {
                        json: Some(
                            serde_json::to_string(&FileSystemRequest {
                                uri_string: "fs://sequencer/blockchain.json".into(),
                                action: FileSystemAction::Read,
                            })
                            .unwrap(),
                        ),
                        bytes: None,
                    },
                },
            ]
            .as_slice(),
            "".into(),
        )));

        let (fs_response, _) = bindings::await_next_message().unwrap();  //  TODO: handle error properly

        bindings::print_to_terminal(1, "sequencer: got blockchain.json");

        let mut pki: OnchainPKI = match fs_response.content.payload.bytes {
            Some(b) => serde_json::from_str::<OnchainPKI>(&String::from_utf8(b).unwrap()).unwrap(),
            None => HashMap::<String, Identity>::new(),
        };

        // bind to blockchain.json path
        bindings::send_requests(Ok((
            vec![
                types::WitProtorequest {
                    is_expecting_response: true, // NOT ACTUALLY THOUGH
                    target: types::WitProcessNode {
                        node: our_name.clone(),
                        process: "http_bindings".into(),
                    },
                    payload: types::WitPayload {
                        json: Some(
                            serde_json::json!({
                                "action": "bind-app",
                                "path": "/blockchain.json", // TODO at some point we need URL pattern matching...later...
                                "app": &process_name,
                            })
                            .to_string(),
                        ),
                        bytes: None,
                    },
                },
            ]
            .as_slice(),
            "".into(),
        )));

        loop {
            let (message, _) = bindings::await_next_message().unwrap();  //  TODO: handle error properly
            let stringy = message.content.payload.json.unwrap_or_default();
            let http_head: serde_json::Value = serde_json::from_str(&stringy).unwrap_or_default();
            if http_head["method"] == "GET" {
                bindings::send_response(Ok((
                    &types::WitPayload {
                        json: Some(
                            serde_json::json!({
                                "action": "response",
                                "id": http_head["id"],
                                "status": 200,
                                "headers": {
                                    "Content-Type": "application/json",
                                },
                            })
                            .to_string(),
                        ),
                        bytes: Some(
                            serde_json::to_string(&pki)
                                .unwrap_or_default()
                                .as_bytes()
                                .to_vec(),
                        ),
                    },
                    "",
                )));
                continue;
            }
            if http_head["method"] != "POST" {
                failure(http_head);
                continue;
            }
            let http_body: IdentityTransaction = match bincode::deserialize::<IdentityTransaction>(
                &message.content.payload.bytes.unwrap_or_default(),
            ) {
                Ok(i) => i,
                Err(e) => {
                    bindings::print_to_terminal(1,
                        format!("sequencer error: failed to deserialize identity: {}", e).as_str(),
                    );
                    failure(http_head);
                    continue;
                }
            };
            let new_identity = match parse_transaction(http_body) {
                Some(i) => i,
                None => {
                    bindings::print_to_terminal(1, "sequencer error: got invalid transaction");
                    failure(http_head);
                    continue;
                }
            };
            pki.insert(new_identity.name.clone(), new_identity);
            bindings::send_response(Ok((
                &types::WitPayload {
                    json: Some(
                        serde_json::json!({
                            "action": "response",
                            "id": http_head["id"],
                            "status": 200,
                            "headers": {
                                "Content-Type": "application/json",
                            },
                        })
                        .to_string(),
                    ),
                    bytes: Some(
                        serde_json::to_string(&pki)
                            .unwrap_or_default()
                            .as_bytes()
                            .to_vec(),
                    ),
                },
                "".into(),
            )));
        }
    }
}
