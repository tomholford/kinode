use crate::types::*;
use ethers::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use ethers::types::transaction::eip2718::TypedTransaction;
use ethers::core::utils::Anvil;
use ethers::middleware::SignerMiddleware;
use ethers::abi::Token;

#[derive(Debug, Serialize, Deserialize)]
enum EthRpcAction {
    Call(EthRpcCall),
    SendTransaction(EthRpcCall),
    DeployContract,
}

#[derive(Debug, Serialize, Deserialize)]
struct EthRpcCall {
    contract_address: String,
    gas: Option<U256>,
    gas_price: Option<U256>,
    // transaction_type // EIP1559, EIP2930, Optimism
}

#[derive(Debug, Serialize, Deserialize)]
struct DeployContract {
    constructor_args: Token, // TODO for some reason Tokens aren't json serializable? fix this
    gas: Option<U256>,
    gas_price: Option<U256>,
}

pub async fn eth_rpc(
    our_name: String,
    send_to_loop: MessageSender,
    mut recv_in_client: MessageReceiver,
    print_tx: PrintSender,
) {
    // Fake chain
    // TODO: use a real chain
    let anvil = Anvil::new().spawn();

    // TODO allow arbitrary wallets, not just [0]. Also arbitrary seeds, external wallets, etc.
    let wallet: LocalWallet = anvil.keys()[0].clone().into();
    
    print_tx.send(Printout {
        verbosity: 0,
        content: format!("eth_rpc: wallet: {:?}", wallet.address()),
    }).await;

    // connect to the network
    let provider = Provider::<Http>::try_from(anvil.endpoint()).unwrap();

    // connect the wallet to the provider
    let client = SignerMiddleware::new(provider, wallet.with_chain_id(anvil.chain_id()));

    while let Some(message) = recv_in_client.recv().await {
        tokio::spawn(handle_message(
            our_name.clone(),
            send_to_loop.clone(),
            message,
            client.clone(),
            print_tx.clone(),
        ));
    }
}

async fn handle_message(
    our: String,
    send_to_loop: MessageSender,
    wm: WrappedMessage,
    client: SignerMiddleware<Provider<Http>, LocalWallet>,
    print_tx: PrintSender,
) {
    print_tx.send(Printout {
        verbosity: 0,
        content: "eth_rpc: got message".to_string(),
    }).await;
    let WrappedMessage { ref id, target: _, ref rsvp, message: Ok(Message { ref source, ref content }), }
            = wm else {
        panic!("eth_rpc: unexpected Error")  //  TODO: implement error handling
    };

    let target = match content.message_type {
        MessageType::Response => panic!("eth_rpc: should not get a response message"),
        MessageType::Request(is_expecting_response) => {
            if is_expecting_response {
                ProcessNode {
                    node: our.clone(),
                    process: source.process.clone(),
                }
            } else {
                let Some(rsvp) = rsvp else { panic!("eth_rpc: no rsvp"); };
                rsvp.clone()
            }
        }
    };

    let Some(json) = content.payload.json.clone() else {
        panic!("eth_rpc: request must have JSON payload, got: {:?}", wm);
    };

    let action: EthRpcAction = match serde_json::from_value(json) {
        Ok(req) => req,
        Err(e) => panic!("eth_rpc: failed to parse request: {:?}", e),
    };

    let call_data = content.payload.bytes.content.clone().unwrap();

    match action {
        EthRpcAction::Call(eth_rpc_call) => {
            let address: Address = eth_rpc_call.contract_address.parse().unwrap(); // TODO unwrap

            let mut call_request = TransactionRequest::new()
                .to(address)
                .data(call_data);
            // gas limit
            call_request = match eth_rpc_call.gas {
                Some(gas) => call_request.gas(gas),
                None => call_request
            };
            // gas price
            call_request = match eth_rpc_call.gas_price {
                Some(gas_price) => call_request.gas_price(gas_price),
                None => call_request
            };

            let call_result = client.call(&TypedTransaction::Legacy(call_request), None).await.unwrap(); // TODO unwrap

            send_to_loop.send(
                WrappedMessage {
                    id: id.clone(),
                    target: target,
                    rsvp: None,
                    message: Ok(Message {
                        source: ProcessNode {
                            node: our,
                            process: "eth_rpc".to_string(),
                        },
                        content: MessageContent {
                            message_type: MessageType::Response,
                            payload: Payload {
                                json: None,
                                bytes: PayloadBytes{
                                    circumvent: Circumvent::False,
                                    content: Some(call_result.as_ref().to_vec()),
                                },
                            },
                        },
                    }),
                }
            ).await.unwrap();
        }
        EthRpcAction::SendTransaction(eth_rpc_call) => {
            let address: Address = eth_rpc_call.contract_address.parse().unwrap(); // TODO unwrap

            let mut call_request = TransactionRequest::new()
                .to(address)
                .data(call_data);
            // gas limit
            call_request = match eth_rpc_call.gas {
                Some(gas) => call_request.gas(gas),
                None => call_request
            };
            // gas price
            call_request = match eth_rpc_call.gas_price {
                Some(gas_price) => call_request.gas_price(gas_price),
                None => call_request
            };

            let pending = client.send_transaction(TypedTransaction::Legacy(call_request), None).await.unwrap(); // TODO unwrap
            let Some(rx) = pending.await.unwrap() else { panic!("foo")}; // TODO unwrap

            send_to_loop.send(
                WrappedMessage {
                    id: id.clone(),
                    target: target,
                    rsvp: None,
                    message: Ok(Message {
                        source: ProcessNode {
                            node: our,
                            process: "eth_rpc".to_string(),
                        },
                        content: MessageContent {
                            message_type: MessageType::Response,
                            payload: Payload {
                                json: None,
                                bytes: PayloadBytes{
                                    circumvent: Circumvent::False,
                                    content: Some(rx.transaction_hash.as_ref().to_vec()), // TODO we should pass back all relevant tx info 
                                },
                            },
                        },
                    }),
                }
            ).await.unwrap();
        }
        EthRpcAction::DeployContract => {
            let factory = ContractFactory::new(Default::default(), call_data.into(), client.clone().into());
            let contract = factory
                .deploy(())  // TODO should pass these in from json arguments
                .unwrap()
                .send()
                .await
                .unwrap();

            let _ = print_tx.send(Printout {
                verbosity: 0,
                content: format!("eth_rpc: deployed"),
            }).await;

            let address = contract.address();

            send_to_loop.send(
                WrappedMessage {
                    id: id.clone(),
                    target: target,
                    rsvp: None,
                    message: Ok(Message {
                        source: ProcessNode {
                            node: our,
                            process: "eth_rpc".to_string(),
                        },
                        content: MessageContent {
                            message_type: MessageType::Response,
                            payload: Payload {
                                json: None,
                                bytes: PayloadBytes{
                                    circumvent: Circumvent::False,
                                    content: Some(address.as_ref().to_vec()),
                                },
                            },
                        },
                    }),
                }
            ).await.unwrap();
        }
    }
}
