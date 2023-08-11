cargo_component_bindings::generate!();

use std::collections::VecDeque;
use serde::{Serialize, Deserialize};
use bindings::{await_next_message, MicrokernelProcess, print_to_terminal, yield_and_await_response};
use bindings::component::microkernel_process::types;

mod process_lib;

struct Component;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum SequentializeRequest {
    QueueMessage { target_node: Option<String>, target_process: String, json: Option<String> },
    RunQueue,
}

enum ReturnStatus {
    Done,
    AcceptNextInput,
}

struct QueueItem {
    target: types::WitProcessNode,
    payload: types::WitPayload,
}

fn handle_message(
    message_queue: &mut VecDeque<QueueItem>,
    our_name: &str,
    process_name: &str,
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
                        target: types::WitProcessNode {
                            node: match target_node {
                                Some(n) => n,
                                None => our_name.into(),
                            },
                            process: target_process,
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
                        let _ = yield_and_await_response(&item.target, &item.payload)?;
                    }
                    Ok(ReturnStatus::Done)
                },
            }
        },
        types::WitMessageType::Response => {
            panic!("{process_name}: got unexpected Response: {:?}", message);
        },
    }
}

impl MicrokernelProcess for Component {
    fn run_process(our_name: String, process_name: String) {
        print_to_terminal(1, "sequentialize: begin");

        let mut message_queue: VecDeque<QueueItem> = VecDeque::new();

        loop {
            match handle_message(
                &mut message_queue,
                &our_name,
                &process_name,
            ) {
                Ok(return_status) => {
                    match return_status {
                        ReturnStatus::Done => return,
                        ReturnStatus::AcceptNextInput => {},
                    }
                },
                Err(e) => {
                    print_to_terminal(0, format!("{}: error: {:?}", process_name, e).as_str());
                    panic!("{}: error: {:?}", process_name, e);
                },
            };
        }
    }
}
