use crate::types::*;
use ethers::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use ethers::types::transaction::eip2718::TypedTransaction;
use ethers::core::utils::Anvil;


#[derive(Debug, Serialize, Deserialize)]
enum EthRpcAction {
    Call(EthRpcCall),
    DeployContract
}

#[derive(Debug, Serialize, Deserialize)]
struct EthRpcCall {
    contract_address: String,
    // gas
    // gas_price
    // wallet_address: String // implicit in the provider
}

#[derive(Debug, Serialize, Deserialize)]
struct DeployContract {
    bytecode: String,
    // gas
    // gas_price
    // wallet_address: String // implicit in the provider
}

pub async fn eth_rpc(
    our_name: String,
    send_to_loop: MessageSender,
    mut recv_in_client: MessageReceiver,
    print_tx: PrintSender,
) {
    let provider = Arc::new(Provider::<Http>::try_from("http://127.0.0.1:8545").unwrap()); // TODO unwrap

    while let Some(message) = recv_in_client.recv().await {
        tokio::spawn(handle_message(
            our_name.clone(),
            send_to_loop.clone(),
            message,
            Arc::clone(&provider),
            print_tx.clone(),
        ));
    }
}

async fn handle_message(
    our: String,
    send_to_loop: MessageSender,
    wm: WrappedMessage,
    provider: Arc<Provider<Http>>,
    print_tx: PrintSender,
) {
    // let anvil = Anvil::new().spawn();
    // let wallet: LocalWallet = anvil.keys()[0].clone().into();

    
    
    // let wallet = LocalWallet::from_bytes(&hex::decode("b7a12fc804aa90096891be2ba806265dba87109b80add8abfdd90ec9fe5fcce9").unwrap());
    // let wallet = wallet.connect(provider, Credentials::Default());
    // let client = SignerMiddleware::new(provider, wallet.with_chain_id(anvil.chain_id()));

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
    print_tx.send(Printout {
        verbosity: 0,
        content: format!("eth_rpc: got calldata: {:?}", call_data),
    }).await;

    match action {
        EthRpcAction::Call(eth_rpc_call) => {
            let address: Address = eth_rpc_call.contract_address.parse().unwrap(); // TODO unwrap

            let call_request = TypedTransaction::Legacy(
                TransactionRequest::new()
                .to(address)
                .data(call_data));
        
        
            print_tx.send(Printout {
                verbosity: 0,
                content: format!("eth_rpc: call_request: {:?}", call_request),
            }).await;
        
            let call_result = provider.call(&call_request, None).await.unwrap(); // TODO unwrap
        
            print_tx.send(Printout {
                verbosity: 0,
                content: format!("eth_rpc: call_result: {:?}", call_result),
            }).await;
        
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
        EthRpcAction::DeployContract => {
            // TODO
            print_tx.send(Printout {
                verbosity: 0,
                content: format!("eth_rpc: deploying..."),
            }).await;

            // let factory = ContractFactory::new(Default::default(), call_data.into(), wallet);
            // let contract = factory
            //     .deploy(()) // TODO should pass these in from json arguments
            //     .unwrap()
            //     .confirmations(0usize)
            //     .send()
            //     .await;

            // print_tx.send(Printout {
            //     verbosity: 0,
            //     content: format!("eth_rpc: deployed"),
            // }).await;
        }
    }
}
