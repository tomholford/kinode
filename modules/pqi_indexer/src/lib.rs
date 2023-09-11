cargo_component_bindings::generate!();

use bindings::component::uq_process::types::*;
use bindings::{print_to_terminal, receive, send_request, send_requests, UqProcess};
use serde::{Deserialize, Serialize};
use serde_json::json;
use alloy_primitives::FixedBytes;
use alloy_sol_types::{sol, SolEnum, SolType, SolCall, SolEvent};
use hex;

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
    event WsChanged(uint256 indexed node, bytes32 publicKey, uint48 ipAndPort,
                    bytes32[] routers);

    event NameRegistered(uint256 indexed node, bytes name, address owner);

    // TODO we have to watch other events but for now this is fine
}

impl UqProcess for Component {
    fn init(our: Address) {
        bindings::print_to_terminal(0, "pqi_indexer: start");

        let event_sub_res = send_requests(&[ // TODO _and_await_response???
            (
                Address{
                    node: our.node.clone(),
                    process: ProcessId::Name("eth_rpc".to_string()),
                },
                Request{
                    inherit: false, // TODO what
                    expects_response: true,
                    metadata: None,
                    ipc: Some(json!({
                        // TODO new deployments
                        // QnsRegistry
                        "SubscribeEvents": {
                            "addresses": null, // TODO fill this in with test deployments ["0xBc56878166877a687a9Ba077D098aea3270f7E1c"],
                            "from_block": 0,
                            "to_block": null,
                            "events": [
                                "NameRegistered(uint256,bytes,address)"
                            ],
                            "topic1": null,
                            "topic2": null,
                            "topic3": null,
                    }}).to_string()),
                },
                None,
                None,
            ),
            (
                Address{
                    node: our.node.clone(),
                    process: ProcessId::Name("eth_rpc".to_string()),
                },
                Request{
                    inherit: false, // TODO what
                    expects_response: true,
                    metadata: None,
                    ipc: Some(json!({
                        // TODO new deployments
                        // PublicResolver
                        "SubscribeEvents": {
                            "addresses": null, // TODO fill this in with test deployments later ["0x3Cfbc06A4FAdE3329aA103FC1dcF709D646F4c36"],
                            "from_block": 0,
                            "to_block": null,
                            "events": [
                                "WsChanged(uint256,bytes32,uint48,bytes32[])",
                            ],
                            "topic1": null,
                            "topic2": null,
                            "topic3": null,
                    }}).to_string()),
                },
                None,
                None,
            )
        ]);

        bindings::print_to_terminal(0, "pqi_indexer: subscribed to events");

        loop {
            let Ok((source, message)) = receive() else {
                print_to_terminal(0, "pqi_indexer: got network error");
                continue;
            };
            let Message::Request(request) = message else {
                print_to_terminal(0, "pqi_indexer: got response");
                continue;
            };
            let Ok(msg) = serde_json::from_str::<AllActions>(&request.ipc.unwrap_or_default()) else {
                print_to_terminal(0, "pqi_indexer: got invalid message");
                continue;
            };

            match msg {
                // anticipating more message types later
                AllActions::EventSubscription(e) => {
                    match decode_hex(&e.topics[0].clone()) {
                        NameRegistered::SIGNATURE_HASH => {
                            bindings::print_to_terminal(0, format!("pqi_indexer: got NameRegistered event: {:?}", e.topics).as_str());
                        }
                        WsChanged::SIGNATURE_HASH => {
                            // bindings::print_to_terminal(0, format!("pqi_indexer: got WsChanged event: {:?}", e).as_str());

                            let node       = &e.topics[1];
                            let decoded    = WsChanged::decode_data(&decode_hex_to_vec(&e.data), true).unwrap();
                            let public_key = hex::encode(decoded.0);
                            let (ip, port) = split_ip_and_port(decoded.1);
                            let routers    = decoded.2;

                            // bindings::print_to_terminal(0, format!("pqi_indexer: NODE: {:?}", node).as_str());
                            // bindings::print_to_terminal(0, format!("pqi_indexer: DECODED: {:?}", decoded).as_str());
                            // bindings::print_to_terminal(0, format!("pqi_indexer: PUB KEY: {:?}", public_key).as_str());
                            // bindings::print_to_terminal(0, format!("pqi_indexer: IP PORT: {:?} {:?}", ip, port).as_str());
                            // bindings::print_to_terminal(0, format!("pqi_indexer: ROUTERS: {:?}", routers).as_str());
                            
                            let json_payload = json!({
                                "PqiUpdate": {
                                    // TODO name
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

                            bindings::print_to_terminal(0, format!("pqi_indexer: JSON {:?}", json_payload).as_str());
                            
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
                            bindings::print_to_terminal(0, format!("pqi_indexer: got unknown event: {:?}", e.topics).as_str());
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