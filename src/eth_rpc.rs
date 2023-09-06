use crate::types::*;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use ethers::core::types::Filter;
use ethers::types::{ValueOrArray, U256};
use ethers::prelude::Provider;
use ethers_providers::{Middleware, Ws, StreamExt};
use serde_json::json;
use thiserror::Error;

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
    event: Option<Vec<String>>, // aka topic0
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
    our: String,
    rpc_url: String,
    send_to_loop: MessageSender,
    mut recv_in_client: MessageReceiver,
    print_tx: PrintSender,
) {
    let Ok(ws_rpc) = Provider::<Ws>::connect(rpc_url).await else {
        panic!("eth_rpc: couldn't connect to ws endpoint");
    };

    // TODO generate subscription IDs and put into here, create a cancel message        
    // TODO maybe don't need to do Arc Mutex
    let subscriptions = Arc::new(Mutex::new(HashMap::<u64, tokio::task::JoinHandle<Result<(), EthRpcError>>>::new()));

    while let Some(message) = recv_in_client.recv().await {
        let our = our.clone();
        let ws_rpc = ws_rpc.clone();
        let send_to_loop = send_to_loop.clone();
        let print_tx = print_tx.clone();

        let KernelMessage {
            id,
            source,
            target: _,
            rsvp,
            message: Message::Request(Request {
                inherit: _,
                expects_response: is_expecting_response,
                ipc: json,
                metadata: _,
            }),
            payload: _,
        } = message else {
            panic!("foo"); // return Err(EthRpcError::Error { error: format!("eth_rpc: couldn't parse message: {:?}", wm) });
        };
    
        let target = if is_expecting_response {
            Address {
                node: our.clone(),
                process: source.process.clone(),
            }
        } else {
            let Some(rsvp) = rsvp else {
                panic!("foo"); // return Err(EthRpcError::Error { error: format!("eth_rpc: got request with no RSVP: {:?}", wm)});
            };
            rsvp.clone()
        };

    
        // let call_data = content.payload.bytes.content.clone().unwrap_or(vec![]);
    
        let Some(json) = json.clone() else {
            panic!("foo"); // return Err(EthRpcError::Error { error: format!("eth_rpc: request must have JSON payload, got: {:?}", wm) });
        };

        let Ok(action) = serde_json::from_str::<EthRpcAction>(&json) else {
            panic!("foo"); // return Err(EthRpcError::Error { error: format!("eth_rpc: couldn't parse json: {:?}", json) });
        };

        match action {
            EthRpcAction::SubscribeEvents(sub) => {
                // print_tx.send(Printout {
                //     verbosity: 0,
                //     content: format!("eth_rpc: target: {:?}", target.clone()),
                // }).await;

                let id: u64 = rand::random();
                send_to_loop.send(
                    KernelMessage {
                        id: id.clone(),
                        source: Address {
                            node: our.clone(),
                            process: ProcessId::Name("eth_rpc".into()),
                        }, // TODO?
                        target: target.clone(), // TODO?
                        rsvp: None,
                        message: Message::Response((
                            Ok(Response {
                                ipc: Some(json!(id).to_string()),
                                metadata: None,
                            }),
                            None,
                        )),
                        payload: None
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
                if let Some(from_block) = sub.from_block {
                    filter = filter.from_block(from_block);
                }
                if let Some(to_block) = sub.to_block {
                    filter = filter.to_block(to_block);
                }
                if let Some(events) = sub.event {
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
                        return Err(EthRpcError::Error { error: format!("eth_rpc: couldn't create event subscription") });
                    };
        
                    while let Some(event) = stream.next().await {
                        // print_tx.send(Printout {
                        //     verbosity: 0,
                        //     content: format!("eth_rpc: got event: {:?}", event),
                        // }).await;
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
                            }
                        ).await.unwrap();
                    }
                    Ok(())
                });
                subscriptions.lock().await.insert(id, handle);
            }
            EthRpcAction::Unsubscribe(sub_id) => {
                print_tx.send(Printout {
                    verbosity: 0,
                    content: format!("eth_rpc: unsubscribing from {}", sub_id),
                }).await;

                if let Some(handle) = subscriptions.lock().await.remove(&sub_id) {
                    handle.abort();
                } else {
                    print_tx.send(Printout {
                        verbosity: 0,
                        content: format!("eth_rpc: no task found with id {}", sub_id),
                    }).await;
                }
            }
        }
    }
}

//
//  helpers
//
// fn make_error_message(
//     our: String,
//     id: u64,
//     source_process: String,
//     error: EthRpcError,
// ) -> WrappedMessage {
//     WrappedMessage {
//         id,
//         target: Address {
//             node: our.clone(),
//             process: source_process,
//         },
//         rsvp: None,
//         message: Err(UqbarError {
//             source: Address {
//                 node: our,
//                 process: "filesystem".into(),
//             },
//             timestamp: get_current_unix_time().unwrap(),  //  TODO: handle error?
//             content: UqbarErrorContent {
//                 kind: error.kind().into(),
//                 // message: format!("{}", error),
//                 message: serde_json::to_value(error).unwrap(),  //  TODO: handle error?
//                 context: serde_json::to_value("").unwrap(),
//             },
//         }),
//     }
// }

// fn get_current_unix_time() -> anyhow::Result<u64> {
//     match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
//         Ok(t) => Ok(t.as_secs()),
//         Err(e) => Err(e.into()),
//     }
// }
