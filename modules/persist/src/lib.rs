cargo_component_bindings::generate!();

use bindings::component::uq_process::types::*;
use bindings::{print_to_terminal, receive, get_payload, Guest, Address, Response, Context, Payload};
use serde::{Deserialize, Serialize};

mod process_lib;

struct Component;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct State {
    val: Option<u64>,
}

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

#[derive(Debug, Serialize, Deserialize)]
enum PersistRequest {
    Get,
    Set { new: u64 },
}

#[derive(Serialize, Deserialize, Debug)]
pub enum FsResponse {
    //  bytes are in payload_bytes
    Read(u128),
    ReadChunk(u128),
    Write(u128),
    Append(u128),
    Delete(u128),
    Length(u64),
    GetState,
    SetState
    //  use FileSystemError
}

impl Guest for Component {
    fn init(our: Address) {
        print_to_terminal(1, "persist: start");

        let mut state = State {
            val: None
        };
        //  can't await yet, match on FsResponse.
        //  refactor to await when available!
        let _ = process_lib::get_state(our.node.clone());

        loop {
            let Ok((source, message)) = receive() else {
                print_to_terminal(0, "persist: got network error");
                continue;
            };

            match message {
                Message::Request(request) => {
                    let persist_msg = serde_json::from_str::<PersistRequest>(&request.clone().ipc.unwrap_or_default());
                    if let Ok(msg) = persist_msg {
                        match msg {
                            PersistRequest::Get => {
                                print_to_terminal(0, &format!("persist: Get state: {:?}", state));
                                continue;
                            },
                            PersistRequest::Set { new } => {
                                print_to_terminal(0, "persist: got Set request");
                                state.val = Some(new);
                                let _ = process_lib::set_state(our.node.clone(), bincode::serialize(&state).unwrap());
                                continue;
                            },
                        }
                    } else {
                        print_to_terminal(0, &format!("persist: got invalid request {:?}", request.clone()));
                        continue;
                    }
                },
                Message::Response((Ok(response), _)) => {
                    let fs_msg = serde_json::from_str::<FsResponse>(&response.clone().ipc.unwrap_or_default());
                    if let Ok(fs_msg) = fs_msg {
                        match fs_msg {
                            FsResponse::GetState => {
                                if let Some(payload) = get_payload() {
                                    if !payload.bytes.is_empty() {
                                        match bincode::deserialize(&payload.bytes) {
                                            Ok(deserialized_state) => state = deserialized_state,
                                            Err(e) => {
                                                print_to_terminal(0, &format!("persist: deserialization error: {:?}", e));
                                                continue;
                                            }
                                        }
                                    }
                                }
                                print_to_terminal(0, &format!("persist: Get state: {:?}", state));

                                continue;
                            },
                            _ => { print_to_terminal(0, "other!"); }
                        }
                    } else {
                        print_to_terminal(0, &format!("persist: got invalid response {:?}", response.clone()));
                        continue;
                    }
                },
                _ => {
                    print_to_terminal(0, "persist: got unexpected message");
                    continue;
                }
            }

        }
    }
}
