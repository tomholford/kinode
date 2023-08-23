// This is an ERC20 Manager app meant to show how to interact with an ERC20 contract
// using alloy and uqbar.
// TODO parse logs to create some sort of state

cargo_component_bindings::generate!();

use bindings::component::microkernel_process::types;
use serde_json::json;
use alloy_primitives::{address, Address, U256};
use alloy_sol_types::{sol, SolEnum, SolType, SolCall};
use serde::{Deserialize, Serialize};

mod process_lib;

const ERC20_COMPILED: &str = include_str!("TestERC20.json");

struct Component;

// examples
// !message tuna eth_demo {"token":"a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48", "method": "TotalSupply"}
// !message tuna eth_demo {"token":"a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48", "method":{"BalanceOf":"8bbe911710c9e592487dde0735db76f83dc44cfd"}}
// !message tuna eth_demo {"token":"a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48", "method":{"Transfer":{"recipient": "8bbe911710c9e592487dde0735db76f83dc44cfd","amount":100}}}
// !message tuna eth_demo {"token":"a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48", "method":{"Approve":{"spender": "8bbe911710c9e592487dde0735db76f83dc44cfd","amount":100}}}
// !message tuna eth_demo {"token":"a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48", "method":{"TransferFrom":{"sender": "8bbe911710c9e592487dde0735db76f83dc44cfd","recipient":"8bbe911710c9e592487dde0735db76f83dc44cfd","amount":100}}}

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
    recipient: String,
    amount: u64, // TODO bignumber
}

#[derive(Debug, Serialize, Deserialize)]
struct Approve {
    spender: String,
    amount: u64, // TODO bignumber
}

#[derive(Debug, Serialize, Deserialize)]
struct TransferFrom {
    sender: String,
    recipient: String,
    amount: u64, // TODO bignumber
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
        let bc: &str = compiled["bytecode"].as_str().unwrap();

        let deployment_res = process_lib::send_request_and_await_response(
            our.clone(),
            "eth_rpc".to_string(),
            Some(json!("DeployContract")),
            types::WitPayloadBytes {
                circumvent: types::WitCircumvent::False,
                content: Some(bc.into())
            },
        );

        bindings::print_to_terminal(0, format!("asdf: {:?}", deployment_res).as_str());

        loop {
            let (message, _) = bindings::await_next_message().unwrap();  //  TODO: handle error properly
            let Some(message_from_loop_string) = message.content.payload.json else {
                panic!("eth demo requires json payload")
            };

            let message_from_loop: Erc20Action = serde_json::from_str(message_from_loop_string.as_str()).unwrap();

            let call_data: Vec<u8> = match message_from_loop.method {
                // views
                //
                Erc20Method::TotalSupply => totalSupplyCall{}.encode(),
                Erc20Method::BalanceOf(addr) => {
                    let adr: Address = addr.as_str().parse().unwrap();
                    balanceOfCall{
                        account: adr
                    }.encode()
                },
                // writes
                //
                Erc20Method::Transfer(transfer) => {
                    let rec: Address = transfer.recipient.as_str().parse().unwrap();
                    transferCall{
                        recipient: rec,
                        amount: U256::from(transfer.amount) // TODO probably need to think about bignumber stuff here
                    }.encode()
                },
                Erc20Method::Approve(approve) => {
                    let addr: Address = approve.spender.as_str().parse().unwrap();
                    approveCall{
                        spender: addr,
                        amount: U256::from(approve.amount) // TODO probably need to think about bignumber stuff here
                    }.encode()
                },
                Erc20Method::TransferFrom(transfer_from) => {
                    let snd: Address = transfer_from.sender.as_str().parse().unwrap();
                    let rec: Address = transfer_from.recipient.as_str().parse().unwrap();

                    transferFromCall{
                        sender: snd,
                        recipient: rec,
                        amount: U256::from(transfer_from.amount) // TODO probably need to think about bignumber stuff here
                    }.encode()
                },
            };
            bindings::print_to_terminal(0, format!("call_data: {:?}", call_data).as_str());
            let res = process_lib::send_request_and_await_response(
                our.clone(),
                "eth_rpc".to_string(),
                Some(json!({
                    "Call": {
                        "contract_address": message_from_loop.token,
                    }
                })),
                types::WitPayloadBytes {
                    circumvent: types::WitCircumvent::False,
                    content: Some(call_data)
                },
            );
            bindings::print_to_terminal(0, format!("response: {:?}", res).as_str());
        }
    }
}
