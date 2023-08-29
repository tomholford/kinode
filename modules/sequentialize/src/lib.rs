cargo_component_bindings::generate!();

use std::collections::VecDeque;
use serde::{Serialize, Deserialize};
use bindings::{await_next_message, MicrokernelProcess, print_to_terminal, send_request_and_await_response};
use bindings::component::microkernel_process::types;

mod process_lib;

struct Component;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessReference {
    pub node: String,
    pub process: ProcessIdentifier,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProcessIdentifier {
    Id(u64),
    Name(String),
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SequentializeRequest {
    QueueMessage {
        target_node: Option<String>,
        target_process: ProcessIdentifier,
        json: Option<String>,
    },
    RunQueue,
}

enum ReturnStatus {
    Done,
    AcceptNextInput,
}

struct QueueItem {
    target: types::WitProcessReference,
    payload: types::WitPayload,
}

fn en_wit_process_identifier(dewit: &ProcessIdentifier) -> types::WitProcessIdentifier {
    match dewit {
        ProcessIdentifier::Id(id) => types::WitProcessIdentifier::Id(id.clone()),
        ProcessIdentifier::Name(name) => types::WitProcessIdentifier::Name(name.clone()),
    }
}

fn handle_message(
    message_queue: &mut VecDeque<QueueItem>,
    our_name: &str,
    // process_name: &str,
) -> anyhow::Result<ReturnStatus> {
    let (message, _context) = await_next_message()?;
    if our_name != message.source.node {
        return Err(anyhow::anyhow!("rejecting foreign Message from {:?}", message.source));
    }
    match message.content.message_type {
        types::WitMessageType::Request(_is_expecting_response) => {
            match process_lib::parse_message_json(message.content.payload.json)? {
                SequentializeRequest::QueueMessage { target_node, target_process, json } => {
                    message_queue.push_back(QueueItem{
                        target: types::WitProcessReference {
                            node: match target_node {
                                Some(n) => n,
                                None => our_name.into(),
                            },
                            identifier: en_wit_process_identifier(&target_process),
                        },
                        payload: types::WitPayload {
                            json,
                            bytes: message.content.payload.bytes,
                        },
                    });
                    Ok(ReturnStatus::AcceptNextInput)
                },
                SequentializeRequest::RunQueue => {
                    for item in message_queue {
                        let _ = send_request_and_await_response(&item.target, &item.payload)?;
                    }
                    Ok(ReturnStatus::Done)
                },
            }
        },
        types::WitMessageType::Response => {
            panic!("sequentialize: got unexpected Response: {:?}", message);
        },
    }
}

impl MicrokernelProcess for Component {
    fn run_process(our: types::WitProcessAddress) {
    // fn run_process(our_name: String, process_name: String) {
        print_to_terminal(1, "sequentialize: begin");

        let mut message_queue: VecDeque<QueueItem> = VecDeque::new();

        loop {
            match handle_message(
                &mut message_queue,
                &our.node,
                // &process_name,
            ) {
                Ok(return_status) => {
                    match return_status {
                        ReturnStatus::Done => return,
                        ReturnStatus::AcceptNextInput => {},
                    }
                },
                Err(e) => {
                    print_to_terminal(0, format!("sequentialize: error: {:?}", e).as_str());
                    panic!("sequentialize: error: {:?}", e);
                },
            };
        }
    }
}
