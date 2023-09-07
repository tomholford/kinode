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
                    let decoded = CreateEntry::decode_data(&decode_hex(&e.data).unwrap(), true).unwrap();
                    
                    
                    bindings::print_to_terminal(0, format!("pqi_indexer: DATA {:?}", f).as_str());
                }
            }
        }
    }
}

// helpers
fn decode_hex(s: &str) -> Result<Vec<u8>, hex::FromHexError> {
    // If the string starts with "0x", skip the prefix
    let hex_part = if s.starts_with("0x") {
        &s[2..]
    } else {
        s
    };
    hex::decode(hex_part)
}
