cargo_component_bindings::generate!();

use bindings::component::uq_process::types::*;
use bindings::{print_to_terminal, receive, send_request, UqProcess};
use serde::{Deserialize, Serialize};
use serde_json::json;
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
    // interface IPQI {
        event CreateEntry(uint64 indexed pqiId, address nftContract, uint256 tokenId,
            bytes32 publicKey, uint48 ipAndPort, uint64[] routers);
        event ModifyEntry(uint64 indexed pqiId, address nftContract, uint256 tokenId,
            bytes32 publicKey, uint48 ipAndPort, uint64[] routers);
    // }
}

impl UqProcess for Component {
    fn init(our: Address) {
        bindings::print_to_terminal(0, "pqi_indexer: start");

        let event_sub_res = send_request( // TODO send_and_await_response
            &Address{
                node: our.node.clone(),
                process: ProcessId::Name("eth_rpc".to_string()),
            },
            &Request{
                inherit: false, // TODO what
                expects_response: true,
                metadata: None,
                ipc: Some(json!({
                    // TODO hardcoded goerli deployments
                    "SubscribeEvents": {
                        "addresses": ["0x83cc06a336cf7B37ed16A94eEE4aFb7644C50842"],
                        "from_block": 14212933,
                        "to_block": null,
                        "events": [
                            "CreateEntry(uint64,address,uint256,bytes32,uint48,uint64[])",
                            "ModifyEntry(uint64,address,uint256,bytes32,uint48,uint64[])",
                        ],
                        "topic1": null,
                        "topic2": null,
                        "topic3": null,
                }}).to_string()),
            },
            None,
            None, 
        );

        // event_sub_res.content.payload.json.map(|json| {
        //     bindings::print_to_terminal(0, format!("event subscription response: {:?}", json).as_str());
        // });

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
                    let pqi_id = hex_to_u64(&e.topics[1].to_string()).unwrap(); // TODO u64

                    let decoded = CreateEntry::decode_data(&decode_hex(&e.data).unwrap(), true).unwrap();
                    let nft_contract = decoded.0;
                    let nft_id       = decoded.1.to_string(); // TODO should probably stay a hex string...
                    let public_key   = hex::encode(decoded.2);
                    let (ip, port)   = split_ip_and_port(decoded.3);
                    let routers      = decoded.4;
                    
                    let json_payload = json!({
                        "pqi_id": pqi_id,
                        "nft_contract": nft_contract.to_string(),
                        "nft_id": nft_id,
                        "public_key": public_key,
                        "ip": ip,
                        "port": port,
                        "routers": routers,
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
            }
        }
    }
}

// helpers
// TODO these probably exist somewhere in alloy...not sure where though.
fn decode_hex(s: &str) -> Result<Vec<u8>, hex::FromHexError> {
    // If the string starts with "0x", skip the prefix
    let hex_part = if s.starts_with("0x") {
        &s[2..]
    } else {
        s
    };
    hex::decode(hex_part)
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