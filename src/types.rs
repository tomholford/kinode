use std::collections::HashMap;
use serde::{Serialize, Deserialize};

use ethers::prelude::*;

pub type CardSender = tokio::sync::mpsc::Sender<Card>;
pub type CardReceiver = tokio::sync::mpsc::Receiver<Card>;

pub type PrintSender = tokio::sync::mpsc::Sender<String>;
pub type PrintReceiver = tokio::sync::mpsc::Receiver<String>;

pub type OnchainPKI = HashMap<H256, Identity>;

#[derive(Debug, Serialize, Deserialize)]
pub struct Identity {
    pub address: H256,
    pub name: String,
    pub ws_url: String,
    pub ws_port: u16,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Card {
    pub source: String,
    pub target: H256,
    pub payload: serde_json::Value,
}

// #[derive(Debug, Serialize, Deserialize)]
// pub struct ID {
//     node: String,
//     app_name: String,
//     app_distributor: String,
//     app_version: String,
// }

pub enum Command {
    Card(Card),
    Quit,
    Invalid,
}
