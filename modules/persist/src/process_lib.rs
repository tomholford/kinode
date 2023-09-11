use serde::{Serialize, Deserialize};

use super::bindings::component::uq_process::types::*;
use super::bindings::{print_to_terminal, send_request, send_and_await_response, Address, Payload};

impl PartialEq for ProcessId {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (ProcessId::Id(i1), ProcessId::Id(i2)) => i1 == i2,
            (ProcessId::Name(s1), ProcessId::Name(s2)) => s1 == s2,
            _ => false,
        }
    }
}
impl PartialEq<&str> for ProcessId {
    fn eq(&self, other: &&str) -> bool {
        match self {
            ProcessId::Id(_) => false,
            ProcessId::Name(s) => s == other,
        }
    }
}
impl PartialEq<u64> for ProcessId {
    fn eq(&self, other: &u64) -> bool {
        match self {
            ProcessId::Id(i) => i == other,
            ProcessId::Name(_) => false,
        }
    }
}

//  TODO when available! await_response, and deserialize into <T> directly.
pub fn get_state(
    our: String,
) {
    //  print_to_terminal(1, "getting persist initial state:");
    send_request(
        &Address {
            node: our,
            process: ProcessId::Name("lfs".to_string()),
        },
        &Request {
            inherit: false,
            expects_response: true,
            ipc: Some(serde_json::to_string(&FsAction::GetState).unwrap()),
            metadata: None,
        },
        None,
        None,
    );
}

pub fn set_state(
    our: String,
    bytes: Vec<u8>,
) {
    //  print_to_terminal(1, "setting state:");
    send_request(
        &Address {
            node: our,
            process: ProcessId::Name("lfs".to_string()),
        },
        &Request {
            inherit: false,
            expects_response: false,
            ipc: Some(serde_json::to_string(&FsAction::SetState).unwrap()),
            metadata: None,
        },
        None,
        Some(&Payload {
            mime: None,
            bytes,
        }),
    );
}


//  move these to better place!
#[derive(Serialize, Deserialize, Debug)]
pub enum FsAction {
    Write,
    Replace(u128),
    Append(Option<u128>),
    Read(u128),
    ReadChunk(ReadChunkRequest),
    Delete(u128),
    Length(u128),
    //  process state management
    GetState,
    SetState,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ReadChunkRequest {
    pub file_uuid: u128,
    pub start: u64,
    pub length: u64,
}