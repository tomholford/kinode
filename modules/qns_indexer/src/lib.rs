cargo_component_bindings::generate!();

use bindings::component::uq_process::types::*;
use bindings::{print_to_terminal, receive, send_request, send_requests, UqProcess};
use serde::{Deserialize, Serialize};
use serde_json::json;
use alloy_primitives::FixedBytes;
use alloy_sol_types::{sol, SolEnum, SolType, SolCall, SolEvent};
use hex;
use std::collections::HashMap;

struct Component;

#[derive(Debug, Serialize, Deserialize)]
enum AllActions {
    EventSubscription(EthEvent),
}

#[derive(Debug, Serialize, Deserialize)]
struct EthEvent {
    address: String,
    blockHash: String,
    blockNumber: String,
    data: String,
    logIndex: String,
    removed: bool,
    topics: Vec<String>,
    transactionHash: String,
    transactionIndex: String,
}

sol! {
    event WsChanged(
        uint256 indexed node,
        uint32 indexed protocols,
        bytes32 publicKey,
        uint48 ipAndPort,
        bytes32[] routers
    );

    event NodeRegistered(uint256 indexed node, bytes name);
}

impl UqProcess for Component {
    fn init(our: Address) {
        bindings::print_to_terminal(0, "qns_indexer: start");

        let mut names: HashMap<String, String> = HashMap::new();

        let event_sub_res = send_request(
                &Address{
                    node: our.node.clone(),
                    process: ProcessId::Name("eth_rpc".to_string()),
                },
                &Request{
                    inherit: false, // TODO what
                    expects_response: true,
                    metadata: None,
                    ipc: Some(json!({
                        // TODO new deployments
                        "SubscribeEvents": {
                            "addresses": [
                                // QNSRegistry on goerli opt
                                "0xb598fe1771DB7EcF2AeD06f082dE1030CA0BF1DA",
                            ],
                            "from_block": 0,
                            "to_block": null,
                            "events": [
                                "NodeRegistered(uint256,bytes)",
                                "WsChanged(uint256,uint32,bytes32,uint48,bytes32[])",
                            ],
                            "topic1": null,
                            "topic2": null,
                            "topic3": null,
                    }}).to_string()),
                },
                None,
                None,
        );

        bindings::print_to_terminal(0, "qns_indexer: subscribed to events");

        loop {
            let Ok((source, message)) = receive() else {
                print_to_terminal(0, "qns_indexer: got network error");
                continue;
            };
            let Message::Request(request) = message else {
                print_to_terminal(0, "qns_indexer: got response");
                continue;
            };
            let Ok(msg) = serde_json::from_str::<AllActions>(&request.ipc.unwrap_or_default()) else {
                print_to_terminal(0, "qns_indexer: got invalid message");
                continue;
            };

            match msg {
                // Probably more message types later...maybe not...
                AllActions::EventSubscription(e) => {
                    match decode_hex(&e.topics[0].clone()) {
                        NodeRegistered::SIGNATURE_HASH => {
                            // bindings::print_to_terminal(0, format!("qns_indexer: got NameRegistered event: {:?}", e).as_str());

                            let node       = &e.topics[1];
                            let decoded    = NodeRegistered::decode_data(&decode_hex_to_vec(&e.data), true).unwrap();
                            let name = dnswire_decode(decoded.0);

                            // bindings::print_to_terminal(0, format!("qns_indexer: NODE1: {:?}", node).as_str());
                            // bindings::print_to_terminal(0, format!("qns_indexer: NAME: {:?}", name.to_string()).as_str());

                            names.insert(node.to_string(), name);
                        }
                        WsChanged::SIGNATURE_HASH => {
                            // bindings::print_to_terminal(0, format!("qns_indexer: got WsChanged event: {:?}", e).as_str());

                            let node       = &e.topics[1];
                            // bindings::print_to_terminal(0, format!("qns_indexer: NODE2: {:?}", node.to_string()).as_str());
                            let decoded     = WsChanged::decode_data(&decode_hex_to_vec(&e.data), true).unwrap();
                            let public_key  = hex::encode(decoded.0);
                            let (ip, port)  = split_ip_and_port(decoded.1);
                            let routers_raw = decoded.2;
                            let routers: Vec<String> = routers_raw
                                .iter()
                                .map(|r| {
                                    let key = hex::encode(r);
                                    match names.get(&key) {
                                        Some(name) => name.clone(),
                                        None => format!("0x{}", key), // TODO it should actually just panic here
                                    }
                                })
                                .collect::<Vec<String>>();

                            let name = names.get(node).unwrap();
                            // bindings::print_to_terminal(0, format!("qns_indexer: NAME: {:?}", name).as_str());
                            // bindings::print_to_terminal(0, format!("qns_indexer: DECODED: {:?}", decoded).as_str());
                            // bindings::print_to_terminal(0, format!("qns_indexer: PUB KEY: {:?}", public_key).as_str());
                            // bindings::print_to_terminal(0, format!("qns_indexer: IP PORT: {:?} {:?}", ip, port).as_str());
                            // bindings::print_to_terminal(0, format!("qns_indexer: ROUTERS: {:?}", routers).as_str());
                            
                            let json_payload = json!({
                                "QnsUpdate": {
                                    "name": name,
                                    "owner": "0x", // TODO or get rid of
                                    "node": node,
                                    "public_key": format!("0x{}", public_key),
                                    "ip": format!(
                                        "{}.{}.{}.{}",
                                        (ip >> 24) & 0xFF,
                                        (ip >> 16) & 0xFF,
                                        (ip >> 8) & 0xFF,
                                        ip & 0xFF
                                    ),
                                    "port": port,
                                    "routers": routers,
                                }
                            }).to_string();

                            // bindings::print_to_terminal(0, format!("qns_indexer: JSON {:?}", json_payload).as_str());
                            
                            send_request(
                                &Address{
                                    node: our.node.clone(),
                                    process: ProcessId::Name("net".to_string()),
                                },
                                &Request{
                                    inherit: false,
                                    expects_response: false,
                                    metadata: None,
                                    ipc: Some(json_payload),
                                },
                                None,
                                None, 
                            );
                        }
                        _ => {
                            bindings::print_to_terminal(0, format!("qns_indexer: got unknown event: {:?}", e).as_str());
                        }
                    }
                }
            }
        }
    }
}

// helpers
// TODO these probably exist somewhere in alloy...not sure where though.
fn decode_hex(s: &str) -> FixedBytes<32> {
    // If the string starts with "0x", skip the prefix
    let hex_part = if s.starts_with("0x") {
        &s[2..]
    } else {
        s
    };

    let mut arr = [0_u8; 32];
    arr.copy_from_slice(&hex::decode(hex_part).unwrap()[0..32]);
    FixedBytes(arr)
}

fn decode_hex_to_vec(s: &str) -> Vec<u8> {
    // If the string starts with "0x", skip the prefix
    let hex_part = if s.starts_with("0x") {
        &s[2..]
    } else {
        s
    };

    hex::decode(hex_part).unwrap()
}

fn hex_to_u64(hex: &str) -> Result<u64, std::num::ParseIntError> {
    let without_prefix = if hex.starts_with("0x") {
        &hex[2..]
    } else {
        hex
    };
    u64::from_str_radix(without_prefix, 16)
}

fn split_ip_and_port(combined: u64) -> (u32, u16) {
    let port = (combined & 0xFFFF) as u16;              // Extract the last 16 bits
    let ip = (combined >> 16) as u32;                   // Right shift to get the first 32 bits
    (ip, port)
}

fn dnswire_decode(wire_format_bytes: Vec<u8>) -> String {
    let mut i = 0;
    let mut result = Vec::new();

    while i < wire_format_bytes.len() {
        let len = wire_format_bytes[i] as usize;
        if len == 0 { break; }
        let end = i + len + 1;
        let mut span = wire_format_bytes[i+1..end].to_vec();
        span.push('.' as u8);
        result.push(span);
        i = end;
    };

    let flat: Vec<_> = result.into_iter().flatten().collect();

    let name = String::from_utf8(flat).unwrap();

    // Remove the trailing '.' if it exists (it should always exist)
    if name.ends_with('.') {
        name[0..name.len()-1].to_string()
    } else {
        name
    }
}