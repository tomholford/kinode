use std::collections::HashMap;
use std::sync::Arc;
use hasher::{Hasher, HasherKeccak};
use cita_trie::MemoryDB;
use cita_trie::{PatriciaTrie, Trie};
use ethers::prelude::*;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateItem {
    pub source: H160,
    pub holder: H160,
    pub town_id: u32,
    pub salt: Bytes,
    pub label: String,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractItem {
    pub source: H160,
    pub holder: H160,
    pub town_id: u32,
    pub code_hex: String, // source code of contract represented as hex string
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub from: H160,
    pub signature: Option<Signature>,
    pub to: H256, // contract address
    pub town_id: u32,
    pub calldata: serde_json::Value,
    pub nonce: U256,
    pub gas_price: U256,
    pub gas_limit: U256,
}

pub struct UqChain {
    state: PatriciaTrie<MemoryDB, HasherKeccak>,
    nonces: HashMap<H160, U256>,
}

impl UqChain {
    fn init() -> UqChain {
        let memdb = Arc::new(MemoryDB::new(true));
        let hasher = Arc::new(HasherKeccak::new());

        let value = StateItem {
                    source: H160::zero(),
                    holder: H160::zero(),
                    town_id: 0,
                    salt: Bytes::new(),
                    label: String::new(),
                    data: serde_json::Value::Null,
                  };

        let key = HasherKeccak::digest(&hasher, &bincode::serialize(&value).unwrap());

        let root = {
            let mut trie = PatriciaTrie::new(Arc::clone(&memdb), Arc::clone(&hasher));
            trie.insert(key.to_vec(), bincode::serialize(&value).unwrap()).unwrap();
            trie.root().unwrap()
        };

        let trie = PatriciaTrie::from(Arc::clone(&memdb), Arc::clone(&hasher), &root).unwrap();

        UqChain {
            state: trie,
            nonces: HashMap::new(),
        }
    }
}

pub fn engine(chain: UqChain, txns: Vec<Transaction>) -> UqChain {
    for txn in txns {
        println!("{:?}", txn)
    }
    chain
}
