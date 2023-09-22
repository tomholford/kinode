use serde::{Serialize, Deserialize};

use super::bindings::component::uq_process::types::*;
use super::bindings::{Address, Payload, get_payload, send_request};

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

pub fn send_and_await_response(
    target: &Address,
    inherit: bool,
    ipc: Option<Json>,
    metadata: Option<Json>,
    payload: Option<&Payload>,
) -> Result<(Address, Message), NetworkError> {
    super::bindings::send_and_await_response(
        target,
        &Request {
            inherit,
            expects_response: true,
            ipc,
            metadata,
        },
        payload,
    )
}

pub fn get_state(
    our: String,
) -> Option<Payload> {
    //  Request/Response stays local -> no NetworkError
    let _ = send_and_await_response(
        &Address {
            node: our,
            process: ProcessId::Name("filesystem".to_string()),
        },
        false,
        Some(serde_json::to_string(&FsAction::GetState).unwrap()),
        None,
        None,
    );
    get_payload()
}

pub fn set_state(
    our: String,
    bytes: Vec<u8>,
) {
    send_request(
        &Address {
            node: our,
            process: ProcessId::Name("filesystem".to_string()),
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

pub fn await_set_state<T>(
    our: String,
    state: &T,
    // bytes: Vec<u8>,
) -> Result<(), UqbarError>
where
    T: serde::Serialize,
{
    //  Request/Response stays local -> no NetworkError
    let (_, response) = send_and_await_response(
        &Address {
            node: our,
            process: ProcessId::Name("filesystem".to_string()),
        },
        false,
        Some(serde_json::to_string(&FsAction::SetState).unwrap()),
        None,
        Some(&Payload {
            mime: None,
            bytes: bincode::serialize(state).unwrap(),
        }),
    ).unwrap();
    match response {
        Message::Request(_) => panic!(""),
        Message::Response((response, _context)) => {
            match response {
                Ok(_) => Ok(()),
                Err(e) => Err(e),
            }
        },
    }
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
