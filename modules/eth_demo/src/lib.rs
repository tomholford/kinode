// This is an ERC20 Manager app meant to show how to interact with an ERC20 contract
// using alloy and uqbar.
// TODO parse logs to create some sort of state

cargo_component_bindings::generate!();

use bindings::component::microkernel_process::types;
use serde_json::json;
use alloy_primitives::{address, Address, U256};
use alloy_sol_types::{sol, SolEnum, SolType, SolCall};
use serde::{Deserialize, Serialize};
use hex;

mod process_lib;

const ERC20_COMPILED: &str = include_str!("TestERC20.json");

struct Component;

// examples

// !message tuna eth_demo {"Subscription":{"id":"asdf"}}
// !message tuna eth_demo {"Erc20Action":{"token":"5fbdb2315678afecb367f032d93f642f64180aa3","method":"TotalSupply"}}
// !message tuna eth_demo {"Erc20Action":{"token":"5fbdb2315678afecb367f032d93f642f64180aa3", "method":{"BalanceOf":"f39fd6e51aad88f6f4ce6ab8827279cfffb92266"}}}
// !message tuna eth_demo {"Erc20Action":{"token":"5fbdb2315678afecb367f032d93f642f64180aa3", "method":{"BalanceOf":"8bbe911710c9e592487dde0735db76f83dc44cfd"}}}
// !message tuna eth_demo {"Erc20Action":{"token":"5fbdb2315678afecb367f032d93f642f64180aa3", "method":{"Transfer":{"recipient": "8bbe911710c9e592487dde0735db76f83dc44cfd","amount":"fff"}}}}
// !message tuna eth_demo {"Erc20Action":{"token":"5fbdb2315678afecb367f032d93f642f64180aa3", "method":{"Approve":{"spender": "8bbe911710c9e592487dde0735db76f83dc44cfd","amount":"fff"}}}}
// !message tuna eth_demo {"Erc20Action":{"token":"5fbdb2315678afecb367f032d93f642f64180aa3", "method":{"TransferFrom":{"sender": "8bbe911710c9e592487dde0735db76f83dc44cfd","recipient":"8bbe911710c9e592487dde0735db76f83dc44cfd","amount":"fff"}}}}

#[derive(Debug, Serialize, Deserialize)]
enum AllActions {
    Erc20Action(Erc20Action),
    BlockSubscription(EthBlock),
}

#[derive(Debug, Serialize, Deserialize)]
struct Erc20Action {
    token: String,
    method: Erc20Method,
}

#[derive(Debug, Serialize, Deserialize)]
enum Erc20Method {
    // views
    TotalSupply,
    BalanceOf(String),
    // writes
    Transfer(Transfer),
    Approve(Approve),
    TransferFrom(TransferFrom),
}

#[derive(Debug, Serialize, Deserialize)]
struct Transfer {
    recipient: String, // no 0x prefix on any of these types
    amount: String, // hex encoded
}

#[derive(Debug, Serialize, Deserialize)]
struct Approve {
    spender: String,
    amount: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct TransferFrom {
    sender: String,
    recipient: String,
    amount: String,
}

// shared with eth_rpc module
#[derive(Debug, Serialize, Deserialize)]
struct EthBlock {
    baseFeePerGas: String,
    difficulty: String,
    extraData: String,
    gasLimit: String,
    gasUsed: String,
    hash: String,
    logsBloom: String,
    miner: String,
    mixHash: String,
    nonce: String,
    number: String,
    parentHash: String,
    receiptsRoot: String,
    sealFields: Vec<String>,
    sha3Uncles: String,
    size: String,
    stateRoot: String,
    timestamp: String,
    totalDifficulty: String,
    transactions: Vec<String>,
    transactionsRoot: String,
    uncles: Vec<String>
}

sol! {
    function totalSupply() external view returns (uint256);
    function balanceOf(address account) external view returns (uint256);
    function transfer(address recipient, uint256 amount) external returns (bool);
    function allowance(address owner, address spender) external view returns (uint256);
    function approve(address spender, uint256 amount) external returns (bool);
    function transferFrom(address sender, address recipient, uint256 amount) external returns (bool);
}

impl bindings::MicrokernelProcess for Component {
    fn run_process(our: String, dap: String) {
        bindings::print_to_terminal(0, "eth-demo: start");

        let compiled: serde_json::Value =
            serde_json::from_str(ERC20_COMPILED).unwrap();

        let bc: Vec<u8> = decode_hex(compiled["bytecode"].as_str().unwrap()).unwrap();

        let deployment_res = process_lib::send_request_and_await_response(
            our.clone(),
            "eth_rpc".to_string(),
            Some(json!("DeployContract")),
            types::WitPayloadBytes {
                circumvent: types::WitCircumvent::False,
                content: Some(bc.into())
            },
        );

        bindings::print_to_terminal(0, format!("ERC20 address: {:?}", hex::encode(deployment_res.unwrap().content.payload.bytes.content.unwrap())).as_str());

        let sub_res = process_lib::send_request_and_await_response(
            our.clone(),
            "eth_rpc".to_string(),
            Some(json!("Subscribe")),
            types::WitPayloadBytes {
                circumvent: types::WitCircumvent::False,
                content: Some(vec![])
            },
        );

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
                    AllActions::Erc20Action(action) => {
                        let (json, call_data): (serde_json::Value, Vec<u8>) = match action.method {
                            // views
                            //
                            Erc20Method::TotalSupply => {
                                (
                                    json!({"Call": {
                                        "contract_address": action.token,
                                        "gas": null,
                                        "gas_price": null,
                                    }}),
                                    totalSupplyCall{}.encode()
                                )
                            },
                            Erc20Method::BalanceOf(addr) => {
                                (
                                    json!({"Call": {
                                        "contract_address": action.token,
                                        "gas": null,
                                        "gas_price": null,
                                    }}),
                                    balanceOfCall{
                                        account: addr.as_str().parse().unwrap()
                                    }.encode()
                                )
                            },
                            // writes
                            //
                            Erc20Method::Transfer(transfer) => {
                                (
                                    json!({"SendTransaction": {
                                        "contract_address": action.token,
                                        "gas": null,
                                        "gas_price": null,
                                    }}),
                                    transferCall{
                                        recipient: transfer.recipient.as_str().parse().unwrap(),
                                        amount: U256::from_str_radix(&transfer.amount, 16).unwrap()
                                    }.encode()
                                )
                            },
                            Erc20Method::Approve(approve) => {
                                bindings::print_to_terminal(0, "h");
                                (
                                    json!({"SendTransaction": {
                                        "contract_address": action.token,
                                        "gas": null,
                                        "gas_price": null,
                                    }}),
                                    approveCall{
                                        spender: approve.spender.as_str().parse().unwrap(),
                                        amount: U256::from_str_radix(&approve.amount, 16).unwrap()
                                    }.encode()
                                )
                            },
                            Erc20Method::TransferFrom(transfer_from) => {
                                bindings::print_to_terminal(0, "i");
                                (
                                    json!({"SendTransaction": {
                                        "contract_address": action.token,
                                        "gas": null,
                                        "gas_price": null,
                                    }}),
                                    transferFromCall{
                                        sender: transfer_from.sender.as_str().parse().unwrap(),
                                        recipient: transfer_from.recipient.as_str().parse().unwrap(),
                                        amount: U256::from_str_radix(&transfer_from.amount, 16).unwrap()
                                    }.encode()
                                )
                            },
                        };
                        bindings::print_to_terminal(0, "l");
                        bindings::print_to_terminal(0, format!("call_data: {:?}", call_data).as_str());
                        let res = process_lib::send_request_and_await_response(
                            our.clone(),
                            "eth_rpc".to_string(),
                            Some(json),
                            types::WitPayloadBytes {
                                circumvent: types::WitCircumvent::False,
                                content: Some(call_data)
                            },
                        );
                        bindings::print_to_terminal(0, format!("response: {:?}", res).as_str());        
                    },
                    AllActions::BlockSubscription(subscription) => {
                        bindings::print_to_terminal(0, format!("subscription: {:?}", subscription).as_str());
                    }
                }
            } else {
                bindings::print_to_terminal(0, format!("eth_demo: failed to parse json message {:?}", message_from_loop_string).as_str());
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
