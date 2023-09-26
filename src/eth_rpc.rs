use crate::types::*;
use anyhow::Result;
use ethers::core::types::Filter;
use ethers::prelude::Provider;
use ethers::types::{ValueOrArray, U256};
use ethers_providers::{Middleware, StreamExt, Ws};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Serialize, Deserialize)]
enum EthRpcAction {
    SubscribeEvents(EthEventSubscription),
    Unsubscribe(u64),
}

#[derive(Debug, Serialize, Deserialize)]
struct EthEventSubscription {
    addresses: Option<Vec<String>>,
    from_block: Option<u64>,
    to_block: Option<u64>,
    events: Option<Vec<String>>, // aka topic0s
    topic1: Option<U256>,
    topic2: Option<U256>,
    topic3: Option<U256>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum EthRpcError {
    NoRsvp,
    BadJson,
    NoJson,
    EventSubscriptionFailed,
}
impl EthRpcError {
    pub fn kind(&self) -> &str {
        match *self {
            EthRpcError::NoRsvp { .. } => "NoRsvp",
            EthRpcError::BadJson { .. } => "BapJson",
            EthRpcError::NoJson { .. } => "NoJson",
            EthRpcError::EventSubscriptionFailed { .. } => "EventSubscriptionFailed",
        }
    }
}

pub async fn eth_rpc(
    our: String,
    rpc_url: String,
    send_to_loop: MessageSender,
    mut recv_in_client: MessageReceiver,
    print_tx: PrintSender,
) -> Result<()> {
    let Ok(ws_rpc) = Provider::<Ws>::connect(rpc_url).await else {
        panic!("eth_rpc: couldn't connect to ws endpoint");
    };

    // TODO maybe don't need to do Arc Mutex
    let subscriptions = Arc::new(Mutex::new(HashMap::<
        u64,
        tokio::task::JoinHandle<Result<(), EthRpcError>>,
    >::new()));

    while let Some(message) = recv_in_client.recv().await {
        let our = our.clone();
        let ws_rpc = ws_rpc.clone();
        let send_to_loop = send_to_loop.clone();
        let print_tx = print_tx.clone();

        let KernelMessage {
            id,
            source,
            ref rsvp,
            message:
                Message::Request(Request {
                    inherit: _,
                    expects_response: is_expecting_response,
                    ipc: json,
                    metadata: _,
                }),
            ..
        } = message
        else {
            panic!("eth_rpc: bad message");
        };

        let target = if is_expecting_response {
            Address {
                node: our.clone(),
                process: source.process.clone(),
            }
        } else {
            let Some(rsvp) = rsvp else {
                send_to_loop
                    .send(make_error_message(
                        our.clone(),
                        id.clone(),
                        source.clone(),
                        EthRpcError::NoRsvp,
                    ))
                    .await
                    .unwrap();
                continue;
            };
            rsvp.clone()
        };

        // let call_data = content.payload.bytes.content.clone().unwrap_or(vec![]);

        let Some(json) = json.clone() else {
            send_to_loop
                .send(make_error_message(
                    our.clone(),
                    id.clone(),
                    source.clone(),
                    EthRpcError::NoJson,
                ))
                .await
                .unwrap();
            continue;
        };

        let Ok(action) = serde_json::from_str::<EthRpcAction>(&json) else {
            send_to_loop
                .send(make_error_message(
                    our.clone(),
                    id.clone(),
                    source.clone(),
                    EthRpcError::BadJson,
                ))
                .await
                .unwrap();
            continue;
        };

        match action {
            EthRpcAction::SubscribeEvents(sub) => {
                let id: u64 = rand::random();
                send_to_loop
                    .send(KernelMessage {
                        id: id.clone(),
                        source: Address {
                            node: our.clone(),
                            process: ProcessId::Name("eth_rpc".into()),
                        },
                        target: target.clone(),
                        rsvp: None,
                        message: Message::Response((
                            Ok(Response {
                                ipc: Some(json!(id).to_string()),
                                metadata: None,
                            }),
                            None,
                        )),
                        payload: None,
                        signed_capabilities: None,
                    })
                    .await
                    .unwrap();

                let mut filter = Filter::new();
                if let Some(addresses) = sub.addresses {
                    filter = filter.address(ValueOrArray::Array(
                        addresses.into_iter().map(|s| s.parse().unwrap()).collect(),
                    ));
                }

                // TODO is there a cleaner way to do all of this?
                if let Some(from_block) = sub.from_block {
                    filter = filter.from_block(from_block);
                }
                if let Some(to_block) = sub.to_block {
                    filter = filter.to_block(to_block);
                }
                if let Some(events) = sub.events {
                    filter = filter.events(&events);
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

                let ws_rpc = ws_rpc.clone();

                let handle = tokio::task::spawn(async move {
                    let Ok(mut stream) = ws_rpc.subscribe_logs(&filter).await else {
                        return Err(EthRpcError::EventSubscriptionFailed);
                    };

                    while let Some(event) = stream.next().await {
                        send_to_loop.send(
                            KernelMessage {
                                id: rand::random(),
                                source: Address {
                                    node: our.clone(),
                                    process: ProcessId::Name("eth_rpc".into()),
                                },
                                target: target.clone(),
                                rsvp: None,
                                message: Message::Request(Request {
                                    inherit: false, // TODO what
                                    expects_response: false,
                                    ipc: Some(json!({
                                        "EventSubscription": serde_json::to_value(event).unwrap()
                                    }).to_string()),
                                    metadata: None,
                                }),
                                payload: None,
                                signed_capabilities: None,
                            }
                        ).await.unwrap();
                    }
                    Ok(())
                });
                subscriptions.lock().await.insert(id, handle);
            }
            EthRpcAction::Unsubscribe(sub_id) => {
                let _ = print_tx
                    .send(Printout {
                        verbosity: 0,
                        content: format!("eth_rpc: unsubscribing from {}", sub_id),
                    })
                    .await;

                if let Some(handle) = subscriptions.lock().await.remove(&sub_id) {
                    handle.abort();
                } else {
                    let _ = print_tx
                        .send(Printout {
                            verbosity: 0,
                            content: format!("eth_rpc: no task found with id {}", sub_id),
                        })
                        .await;
                }
            }
        }
    }

    Ok(())
}

//
//  helpers
//

fn make_error_message(our: String, id: u64, source: Address, error: EthRpcError) -> KernelMessage {
    KernelMessage {
        id,
        source: Address {
            node: our.clone(),
            process: ProcessId::Name("eth_rpc".into()),
        },
        target: source,
        rsvp: None,
        message: Message::Response((
            Err(UqbarError {
                kind: error.kind().into(),
                message: Some(serde_json::to_string(&error).unwrap()),
            }),
            None,
        )),
        payload: None,
        signed_capabilities: None,
    }
}
