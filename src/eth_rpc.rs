use crate::types::*;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use ethers::prelude::*;
use serde::{Deserialize, Serialize};
use ethers::types::transaction::eip2718::TypedTransaction;
use ethers::core::utils::Anvil;
use ethers::core::types::Filter;
use ethers::middleware::SignerMiddleware;
use ethers::abi::Token;
use ethers_providers::Ws;
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Serialize, Deserialize)]
enum EthRpcAction {
    SubscribeBlocks,
    SubscribeEvents(EthEventSubscription),
    Unsubscribe(u64),
}

#[derive(Debug, Serialize, Deserialize)]
struct EthEventSubscription {
    addresses: Option<Vec<String>>,
    event: Option<String>, // aka topic0. TODO subscribing to multiple events...
    topic1: Option<U256>,
    topic2: Option<U256>,
    topic3: Option<U256>,
}

#[derive(Error, Debug, Serialize, Deserialize)]
pub enum EthRpcError {
    #[error("eth_rpc: error {error}")]
    Error { error: String },
    // TODO fill these out
}
impl EthRpcError {
    pub fn kind(&self) -> &str {
        match *self {
            EthRpcError::Error { .. } => "error",     
        }
    }
}

pub async fn eth_rpc(
    our_name: String,
    send_to_loop: MessageSender,
    mut recv_in_client: MessageReceiver,
    print_tx: PrintSender,
) {
    // Fake chain
    // TODO: use a real chain
    let anvil = Anvil::new().spawn(); // TODO goerli, optimism

    let Ok(ws_rpc) = Provider::<Ws>::connect(anvil.ws_endpoint()).await else {
        panic!("eth_rpc: couldn't connect to ws endpoint");
    };

    // TODO generate subscription IDs and put into here, create a cancel message        
    let subscription_threads = Arc::new(Mutex::new(HashMap::<u64, tokio::task::JoinHandle<()>>::new()));

    while let Some(message) = recv_in_client.recv().await {
        let id = message.id.clone();
        let our_name = our_name.clone();
        let send_to_loop = send_to_loop.clone();
        let ws_rpc = ws_rpc.clone();
        let print_tx = print_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_message(
                our_name.clone(),
                send_to_loop.clone(),
                message,
                ws_rpc,
                print_tx,
            ).await {
                send_to_loop.send(
                    make_error_message(
                        our_name,
                        id,
                        "eth_rpc".to_string(),
                        e,
                    )
                ).await.unwrap();
            }
        });
    }
}

async fn handle_message(
    our: String,
    send_to_loop: MessageSender,
    wm: WrappedMessage,
    ws_rpc: Provider<Ws>,
    print_tx: PrintSender,
) -> Result<(), EthRpcError>  {
    print_tx.send(Printout {
        verbosity: 0,
        content: "eth_rpc: got message".to_string(),
    }).await;
    let WrappedMessage { ref id, target: _, ref rsvp, message: Ok(Message { ref source, ref content }), }
        = wm else {
            return Err(EthRpcError::Error { error: format!("eth_rpc: couldn't parse message: {:?}", wm) });
        };

    let target = match content.message_type {
        MessageType::Response => {
            return Err(EthRpcError::Error { error: format!("eth_rpc: got response, expected request: {:?}", wm) });
        },
        MessageType::Request(is_expecting_response) => {
            if is_expecting_response {
                ProcessNode {
                    node: our.clone(),
                    process: source.process.clone(),
                }
            } else {
                let Some(rsvp) = rsvp else {
                    return Err(EthRpcError::Error { error: format!("eth_rpc: got request with no RSVP: {:?}", wm)});
                };
                rsvp.clone()
            }
        }
    };

    let call_data = content.payload.bytes.content.clone().unwrap_or(vec![]);

    let Some(json) = content.payload.json.clone() else {
        return Err(EthRpcError::Error { error: format!("eth_rpc: request must have JSON payload, got: {:?}", wm) });
    };

    let Ok(action) = serde_json::from_value::<EthRpcAction>(json.clone()) else {
        return Err(EthRpcError::Error { error: format!("eth_rpc: couldn't parse json: {:?}", json) });
    };

    match action {
        EthRpcAction::SubscribeBlocks => {
            // TODO must make this a tokio thread so that we can cancel it
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

            let mut stream = ws_rpc.subscribe_blocks().await.unwrap();

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
                                    json: Some(json!({
                                        "BlockSubscription": serde_json::to_value(block).unwrap()
                                    })),
                                    bytes: PayloadBytes{
                                        circumvent: Circumvent::False,
                                        content: None
                                    },
                                },
                            },
                        }),
                    }
                ).await.unwrap();
            }
        }
        EthRpcAction::SubscribeEvents(sub) => {
            print_tx.send(Printout {
                verbosity: 0,
                content: format!("eth_rpc: subscribing to events: {:?}", sub),
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

            let mut filter = Filter::new();
            if let Some(addresses) = sub.addresses {
                filter = filter.address(ValueOrArray::Array(
                    addresses
                        .into_iter()
                        .map(|s| s.parse().unwrap())
                        .collect()
                ));
            }

            // TODO is there a cleaner way to do all of this?
            if let Some(event) = sub.event {
                filter = filter.event(&event);
            }
            if let Some(topic1) = sub.topic1 {
                filter = filter.topic1(topic1);
            }
            if let Some(topic2) = sub.topic2 {
                filter = filter.topic2(topic2);
            }
            if let Some(topic3) = sub.topic3 {
                filter = filter.topic3(topic3);
            }

            let Ok(mut stream) = ws_rpc.subscribe_logs(&filter).await else {
                return Err(EthRpcError::Error { error: format!("eth_rpc: couldn't create event subscription") });
            };

            while let Some(event) = stream.next().await {
                print_tx.send(Printout {
                    verbosity: 0,
                    content: format!("eth_rpc: got event: {:?}", event),
                }).await;
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
                                    json: Some(json!({
                                        "EventSubscription": serde_json::to_value(event).unwrap()
                                    })),
                                    bytes: PayloadBytes{
                                        circumvent: Circumvent::False,
                                        content: None
                                    },
                                },
                            },
                        }),
                    }
                ).await.unwrap();
            }
        }
        EthRpcAction::Unsubscribe(sub_id) => {
            // TODO
            print_tx.send(Printout {
                verbosity: 0,
                content: format!("eth_rpc: unsubscribing: {:?}", sub_id),
            }).await;
        }
    }

    Ok(())
}

//
//  helpers
//
fn make_error_message(
    our_name: String,
    id: u64,
    source_process: String,
    error: EthRpcError,
) -> WrappedMessage {
    WrappedMessage {
        id,
        target: ProcessNode {
            node: our_name.clone(),
            process: source_process,
        },
        rsvp: None,
        message: Err(UqbarError {
            source: ProcessNode {
                node: our_name,
                process: "filesystem".into(),
            },
            timestamp: get_current_unix_time().unwrap(),  //  TODO: handle error?
            content: UqbarErrorContent {
                kind: error.kind().into(),
                // message: format!("{}", error),
                message: serde_json::to_value(error).unwrap(),  //  TODO: handle error?
                context: serde_json::to_value("").unwrap(),
            },
        }),
    }
}

fn get_current_unix_time() -> anyhow::Result<u64> {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(t) => Ok(t.as_secs()),
        Err(e) => Err(e.into()),
    }
}
