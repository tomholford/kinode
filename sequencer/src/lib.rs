use bindings::component::microkernel_process::types::WitRequestTypeWithTarget;
use cita_trie::MemoryDB;
use cita_trie::PatriciaTrie;
use hasher::{Hasher, HasherKeccak};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;

struct Component;

//
// chain types
//

#[derive(Debug, Serialize, Deserialize)]
struct SequencerState {
    pub last_batch: u64,
    pub last_batch_hash: String,
    pub mempool: Vec<Transaction>,
}

#[derive(Debug, Serialize, Deserialize)]
enum SequencerMessage {
    MakeBatch,
    Transaction(Transaction),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum Item {
    State(StateItem),
    Contract(ContractItem),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StateItem {
    pub source: String,
    pub holder: String,
    pub town_id: u32,
    pub salt: u32,
    pub label: String,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ContractItem {
    pub source: String,
    pub holder: String,
    pub town_id: u32,
    pub code_hex: String, // source code of contract represented as hex string
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Transaction {
    pub from: String,
    pub signature: Option<String>,
    pub to: String, // contract address
    pub town_id: u32,
    pub calldata: serde_json::Value,
    pub nonce: String,
    pub gas_price: String,
    pub gas_limit: String,
}

struct UqChain {
    state: PatriciaTrie<MemoryDB, HasherKeccak>,
    nonces: HashMap<String, String>, // map of address to current nonce
}

//
// app logic
//

impl bindings::MicrokernelProcess for Component {
    fn run_process(our_name: String, process_name: String) {
        bindings::print_to_terminal("sequencer: initialized");

        let mut state = SequencerState {
            last_batch: 0,
            last_batch_hash: "".to_string(),
            mempool: vec![],
        };

        loop {
            let mut message_stack = bindings::await_next_message();
            let message = message_stack.pop().unwrap();
            let Some(message_from_loop_string) = message.payload.json else {
                bindings::print_to_terminal("sequencer: needed json payload");
                continue;
            };
            let Ok(msg) = serde_json::from_str::<SequencerMessage>(&message_from_loop_string) else {
                bindings::print_to_terminal("sequencer: couldn't parse json payload");
                continue;
            };
            match msg {
                SequencerMessage::MakeBatch => {
                    bindings::print_to_terminal("sequencer: making batch");
                },
                SequencerMessage::Transaction(txn) => {
                    bindings::print_to_terminal("sequencer: got txn");
                    state.mempool.push(txn);
                },
            }
        }
    }
}

bindings::export!(Component);
