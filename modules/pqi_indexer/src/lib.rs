cargo_component_bindings::generate!();

use bindings::component::microkernel_process::types;
use serde_json::json;
use serde::{Deserialize, Serialize};

mod process_lib;

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
    transactionLogIndex: String,
}

// sol! {
//     function totalSupply() external view returns (uint256);
//     function balanceOf(address account) external view returns (uint256);
//     function transfer(address recipient, uint256 amount) external returns (bool);
//     function allowance(address owner, address spender) external view returns (uint256);
//     function approve(address spender, uint256 amount) external returns (bool);
//     function transferFrom(address sender, address recipient, uint256 amount) external returns (bool);
// }

impl bindings::MicrokernelProcess for Component {
    fn run_process(our: String, dap: String) {
        bindings::print_to_terminal(0, "pqi_indexer: start");

        let event_sub_res = process_lib::send_request_and_await_response(
            our.clone(),
            "eth_rpc".to_string(),
            Some(json!({
                // TODO hardcoded goerli deployments
                "SubscribeEvents": {
                    "addresses": ["0x83cc06a336cf7B37ed16A94eEE4aFb7644C50842"],
                    "events": [
                        "CreateEntry(uint64,address,uint256,bytes32,uint48,uint64[])",
                        "ModifyEntry(uint64,address,uint256,bytes32,uint48,uint64[])",
                    ],
                    "topic1": null,
                    "topic2": null,
                    "topic3": null,
                }})),
            types::WitPayloadBytes {
                circumvent: types::WitCircumvent::False,
                content: None
            },
        );

        // event_sub_res.content.payload.json.map(|json| {
        //     bindings::print_to_terminal(0, format!("event subscription response: {:?}", json).as_str());
        // });

        bindings::print_to_terminal(0, "eth-demo: subscribed to events");

        loop {
            bindings::print_to_terminal(0, "a");
            let (message, _) = bindings::await_next_message().unwrap();  //  TODO: handle error properly
            bindings::print_to_terminal(0, "b");
            let Some(message_from_loop_string) = message.content.payload.json else {
                bindings::print_to_terminal(0, "eth demo requires json payload");
                return
            };

            if let Ok(message_from_loop) = serde_json::from_str::<AllActions>(message_from_loop_string.as_str()) {
                match message_from_loop {
                    AllActions::EventSubscription(subscription) => {
                        bindings::print_to_terminal(0, format!("event subscription: {:?}", subscription).as_str());
                    }
                    _ => {
                        bindings::print_to_terminal(0, format!("eth_demo: unknown message {:?}", message_from_loop_string).as_str());
                    }
                }
            } else {
                bindings::print_to_terminal(0, format!("eth_demo: failed to parse json message {:?}", message_from_loop_string).as_str());
            }
        }
    }
}
