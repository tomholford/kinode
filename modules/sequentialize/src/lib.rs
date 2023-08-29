cargo_component_bindings::generate!();

use std::collections::VecDeque;
use serde::{Serialize, Deserialize};
use bindings::{MicrokernelProcess, print_to_terminal, receive, send_and_await_receive};
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
    target: types::ProcessReference,
    payload: types::OutboundPayload,
}

fn en_wit_process_identifier(dewit: &ProcessIdentifier) -> types::ProcessIdentifier {
    match dewit {
        ProcessIdentifier::Id(id) => types::ProcessIdentifier::Id(id.clone()),
        ProcessIdentifier::Name(name) => types::ProcessIdentifier::Name(name.clone()),
    }
}

fn handle_message(
    message_queue: &mut VecDeque<QueueItem>,
    our_name: &str,
) -> anyhow::Result<ReturnStatus> {
    let (message, _context) = receive()?;

    match message {
        types::InboundMessage::Request(types::InboundRequest {
            is_expecting_response: _,
            payload: types::InboundPayload {
                source,
                json,
                bytes,
            },
        }) => {
            if our_name != source.node {
                return Err(anyhow::anyhow!(
                    "rejecting foreign Message from {:?}",
                    source,
                ));
            }
            match process_lib::parse_message_json(json)? {
                SequentializeRequest::QueueMessage { target_node, target_process, json } => {
                    message_queue.push_back(QueueItem{
                        target: types::ProcessReference {
                            node: match target_node {
                                Some(n) => n,
                                None => our_name.into(),
                            },
                            identifier: en_wit_process_identifier(&target_process),
                        },
                        payload: types::OutboundPayload {
                            json,
                            bytes: process_lib::make_outbound_bytes_from_noncircumvented_inbound(bytes)?,
                        },
                    });
                    Ok(ReturnStatus::AcceptNextInput)
                },
                SequentializeRequest::RunQueue => {
                    for item in message_queue {
                        let _ = send_and_await_receive(&item.target, &item.payload)?;
                    }
                    Ok(ReturnStatus::Done)
                },
            }
        },
        types::InboundMessage::Response(_) => {
            panic!("sequentialize: got unexpected Response: {:?}", message);
        },
    }
}

impl MicrokernelProcess for Component {
    fn run_process(our: types::ProcessAddress) {
        print_to_terminal(1, "sequentialize: begin");

        let mut message_queue: VecDeque<QueueItem> = VecDeque::new();

        loop {
            match handle_message(
                &mut message_queue,
                &our.node,
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
