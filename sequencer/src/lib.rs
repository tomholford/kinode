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
    fn init(_source_ship: String, _source_app: String) -> Vec<bindings::WitMessage> {
        bindings::set_state(serde_json::to_string(&json!([])).unwrap().as_str());
        bindings::print_to_terminal("sequencer: initialized");
        vec![]
    }

    fn run_write(
        mut message_stack: Vec<bindings::WitMessage>,
    ) -> Vec<(bindings::WitMessageTypeWithTarget, bindings::WitPayload)> {
        let message = message_stack.pop().unwrap();
        let Some(message_from_loop) = message.payload.json else {
            panic!("foo")
        };
        let mut response_string = "\"".to_string();
        response_string.push_str(&message_from_loop);
        response_string.push_str(" appended by poast\"");
        let response = bindings::component::microkernel_process::types::WitPayload {
            json: Some(response_string.clone()),
            bytes: None,
        };
        let state_string = bindings::fetch_state("");
        let mut state = serde_json::from_str(&state_string).unwrap();
        state = match state {
            serde_json::Value::Array(mut vector) => {
                vector.push(serde_json::to_value(response_string.clone()).unwrap());
                serde_json::Value::Array(vector)
            }
            _ => json!([response_string.clone()]), // TODO
        };
        bindings::set_state(serde_json::to_string(&state).unwrap().as_str());
        // bindings::to_event_loop(
        //     &message.wire.source_ship.clone(),
        //     &"http_server".to_string(),
        //     bindings::WitMessageType::Request(false),
        //     &response
        // );
        vec![(
            bindings::WitMessageTypeWithTarget::Request(WitRequestTypeWithTarget {
                is_expecting_response: false,
                target_ship: message.wire.source_ship.clone(),
                target_app: "http_server".to_string(),
            }),
            response,
        )]
    }

    fn run_read(
        _message_stack: Vec<bindings::WitMessage>,
    ) -> Vec<(bindings::WitMessageType, bindings::WitPayload)> {
        vec![]
    }

    fn handle_response(
        _message_stack: Vec<bindings::WitMessage>,
    ) -> Vec<(bindings::WitMessageTypeWithTarget, bindings::WitPayload)> {
        bindings::print_to_terminal("in handle_response");
        vec![]
    }
}

bindings::export!(Component);
