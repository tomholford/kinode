cargo_component_bindings::generate!();

use bindings::component::uq_process::types::*;
use bindings::{get_payload, print_to_terminal, receive, send_request, send_response, Guest};

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
mod process_lib;

struct Component;

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

fn parse_transaction(tx: IdentityTransaction) -> Option<Identity> {
    // TODO
    if tx.from != tx.calldata.address {
        return None;
    }
    return Some(tx.calldata);
}

fn failure(http_head: serde_json::Value) {
    send_response(
        &Response {
            ipc: Some(
                json!({
                    "action": "response",
                    "id": http_head["id"],
                    "status": 400,
                    "headers": {
                        "Content-Type": "application/json",
                    },
                })
                .to_string(),
            ),
            metadata: None,
        },
        None,
    );
}

impl Guest for Component {
    fn init(our: Address) {
        assert!(our.process == ProcessId::Name("sequencer".into()));
        print_to_terminal(0, "sequencer: initialized");

        let mut pki: OnchainPKI = match process_lib::get_state(our.node.clone()) {
            None => HashMap::<String, Identity>::new(),
            Some(p) => match bincode::deserialize::<HashMap<String, Identity>>(&p.bytes) {
                Err(e) => {
                    print_to_terminal(
                        0,
                        &format!("sequencer: failed to deserialize state from fs: {}", e),
                    );
                    HashMap::<String, Identity>::new()
                }
                Ok(s) => s,
            },
        };

        // bind to blockchain.json path
        send_request(
            &Address {
                node: our.node.clone(),
                process: ProcessId::Name("http_bindings".into()),
            },
            &Request {
                inherit: false,
                expects_response: false,
                ipc: Some(
                    serde_json::json!({
                        "action": "bind-app",
                        "path": "/sequencer/blockchain.json",
                        "app": "sequencer",
                        "authenticated": false,
                    })
                    .to_string(),
                ),
                metadata: None,
            },
            None,
            None,
        );

        loop {
            let (_source, message) = receive().unwrap(); //  TODO: handle error properly
            let Message::Request(request) = message else {
                continue // ignore all responses
            };
            let stringy = request.ipc.unwrap_or_default();
            let http_head: serde_json::Value = serde_json::from_str(&stringy).unwrap_or_default();
            if http_head["method"] == "GET" {
                send_response(
                    &Response {
                        ipc: Some(
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
                        metadata: None,
                    },
                    Some(&Payload {
                        mime: Some("application/json".into()),
                        bytes: serde_json::to_string(&pki)
                            .unwrap_or_default()
                            .as_bytes()
                            .to_vec(),
                    }),
                );
                continue;
            }
            if http_head["method"] != "POST" {
                failure(http_head);
                continue;
            }
            let http_body: IdentityTransaction =
                match bincode::deserialize::<IdentityTransaction>(&get_payload().unwrap().bytes) {
                    Ok(i) => i,
                    Err(e) => {
                        print_to_terminal(
                            1,
                            format!("sequencer error: failed to deserialize identity: {}", e)
                                .as_str(),
                        );
                        failure(http_head);
                        continue;
                    }
                };
            let new_identity = match parse_transaction(http_body) {
                Some(i) => i,
                None => {
                    print_to_terminal(1, "sequencer error: got invalid transaction");
                    failure(http_head);
                    continue;
                }
            };
            let new_id_name = new_identity.name.clone();
            pki.insert(new_id_name.clone(), new_identity);
            match process_lib::await_set_state(our.node.clone(), &pki) {
                Err(e) => {
                    print_to_terminal(0, &format!("persist: failed to set state: {:?}", e));
                    failure(http_head);
                }
                Ok(_) => {
                    print_to_terminal(
                        0,
                        &format!("sequencer: added PKI entry {}", new_id_name),
                    );
                    send_response(
                        &Response {
                            ipc: Some(
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
                            metadata: None,
                        },
                        Some(&Payload {
                            mime: Some("application/json".into()),
                            bytes: serde_json::to_string(&pki)
                                .unwrap_or_default()
                                .as_bytes()
                                .to_vec(),
                        }),
                    );
                }
            }
        }
    }
}
