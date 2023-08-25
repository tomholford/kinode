use crate::types::*;
use ethers::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use ethers::types::transaction::eip2718::TypedTransaction;
use ethers::core::utils::Anvil;
use ethers::middleware::SignerMiddleware;
use ethers::abi::Token;
use ethers_providers::Ws;
use serde_json::json;

#[derive(Debug, Serialize, Deserialize)]
enum EthRpcAction {
    Call(EthRpcCall),
    SendTransaction(EthRpcCall),
    DeployContract,
    Subscribe // TODO to specific events
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

#[derive(Debug, Serialize, Deserialize)]
struct EthRpcSubscription {
    // TODO these are just random fields
    id: String,
}

pub async fn eth_rpc(
    our_name: String,
    send_to_loop: MessageSender,
    mut recv_in_client: MessageReceiver,
    print_tx: PrintSender,
) {
    // Fake chain
    // TODO: use a real chain
    let anvil = Anvil::new().spawn(); // TODO goerli

    // TODO allow arbitrary wallets, not just [0]. Also arbitrary seeds, external wallets, etc.
    let wallet: LocalWallet = anvil.keys()[0].clone().into();
    
    print_tx.send(Printout {
        verbosity: 0,
        content: format!("eth_rpc: wallet: {:?}", wallet.address()),
    }).await;

    // connect to the network
    let provider = Provider::<Http>::try_from(anvil.endpoint()).unwrap();
    let client = SignerMiddleware::new(provider, wallet.with_chain_id(anvil.chain_id()));

    let subscriptions = Provider::<Ws>::connect(anvil.ws_endpoint()).await.unwrap();

    while let Some(message) = recv_in_client.recv().await {
        // TODO generate subscription IDs and put this into a hashmap and create a cancel message
        let handle = tokio::spawn(handle_message(
            our_name.clone(),
            send_to_loop.clone(),
            message,
            subscriptions.clone(),
            client.clone(),
            print_tx.clone(),
        ));
    }
}

async fn handle_message(
    our: String,
    send_to_loop: MessageSender,
    wm: WrappedMessage,
    subscriptions: Provider<Ws>,
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

    let call_data = content.payload.bytes.content.clone().unwrap_or(vec![]);

    if let Ok(action) = serde_json::from_value::<EthRpcAction>(json.clone()) {
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
            EthRpcAction::Subscribe => {
                print_tx.send(Printout {
                    verbosity: 0,
                    content: "eth_rpc: subscribing to blocks".to_string(),
                }).await;
    
                // have to send this empty response to make process happy
                send_to_loop.send(
                    WrappedMessage {
                        id: id.clone(),
                        target: target.clone(),
                        rsvp: None,
                        message: Ok(Message {
                            source: ProcessNode {
                                node: our.clone(),
                                process: "eth_rpc".to_string(),
                            },
                            content: MessageContent {
                                message_type: MessageType::Response,
                                payload: Payload {
                                    json: None,
                                    bytes: PayloadBytes{
                                        circumvent: Circumvent::False,
                                        content: Some(vec![]),
                                    },
                                },
                            },
                        }),
                    }
                ).await.unwrap();
    
                let mut stream = subscriptions.subscribe_blocks().await.unwrap();
    
                // send to target
                // until they cancel
    
                while let Some(block) = stream.next().await {
                    send_to_loop.send(
                        WrappedMessage {
                            id: rand::random(),
                            target: target.clone(),
                            rsvp: None,
                            message: Ok(Message {
                                source: ProcessNode {
                                    node: our.clone(),
                                    process: "eth_rpc".to_string(),
                                },
                                content: MessageContent {
                                    message_type: MessageType::Request(false),
                                    payload: Payload {
                                        // TODO figure out a json format for subscriptions
                                        json: Some(json!({
                                            "Subscription": {
                                                "id": rand::random::<u64>().to_string(),
                                            }
                                        })),
                                        bytes: PayloadBytes{
                                            circumvent: Circumvent::False,
                                            content: Some(block.hash.unwrap().as_ref().to_vec()),
                                        },
                                    },
                                },
                            }),
                        }
                    ).await.unwrap();
                }
            }
        }
    } else {
        print_tx.send(Printout {
            verbosity: 0,
            content: format!("eth_rpc: couldn't parse json message: {:?}", json),
        }).await;
    }
}
