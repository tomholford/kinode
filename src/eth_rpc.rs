use crate::types::*;
use ethers::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Serialize, Deserialize)]
struct EthRpcCall {
    method: String,
    params: serde_json::Value,
}

pub async fn eth_rpc(
    our_name: String,
    send_to_loop: MessageSender,
    mut recv_in_client: MessageReceiver,
    print_tx: PrintSender,
) {
    let provider = Arc::new(Provider::<Http>::try_from("http://127.0.0.1:8545").unwrap()); // TODO unwrap
    // let wallet: LocalWallet = "0x9c5ddaad21ac0bb8033b544eef0057275cb170f04fc8afba90093729c9ae0ebb".parse().unwrap(); // TODO unhardcode
    // let wallet = wallet.connect(provider); // TODO multiple wallets

    while let Some(message) = recv_in_client.recv().await {
        tokio::spawn(handle_message(
            our_name.clone(),
            send_to_loop.clone(),
            message,
            Arc::clone(&provider),
            // wallet,
            print_tx.clone(),
        ));
    }
}

async fn handle_message(
    our: String,
    send_to_loop: MessageSender,
    wm: WrappedMessage,
    provider: Arc<Provider<Http>>,
    // wallet: LocalWallet,
    print_tx: PrintSender,
) {
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

    let Some(value) = content.payload.json.clone() else {
        panic!("eth_rpc: request must have JSON payload, got: {:?}", wm);
    };

    let req: EthRpcCall = match serde_json::from_value(value) {
        Ok(req) => req,
        Err(e) => panic!("eth_rpc: failed to parse request: {:?}", e),
    };

    let handle = provider.get_block(BlockNumber::Latest).await.unwrap_or(None);

    // let recipient: Address = "0xFa99DD35A47f34CBB5d1d0951820040Ea36d8206".parse().unwrap();
    // let amount = ethers::utils::parse_ether("0.01").unwrap();
    // let tx = TransactionRequest::pay(recipient, amount);
    // let pending_tx = wallet.send_transaction(tx, None).await.unwrap();
    // let receipt = pending_tx.await.unwrap();
    
    // TODO accept any arbitrary RPC call (method, params)
    let _ = print_tx.send(
        Printout {
            verbosity: 0,
            content: format!("eth_rpc: sent tx: {:?}", handle),
        }
    ).await;
}
