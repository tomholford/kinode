cargo_component_bindings::generate!();
mod process_lib;
struct Component;
use bindings::{component::uq_process::types::*, Guest};

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

mod kernel_types;

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
enum SequentializeRequest {
    QueueMessage(QueueMessage),
    RunQueue,
}

#[derive(Serialize, Deserialize)]
struct QueueMessage {
    target: kernel_types::ProcessId,
    request: kernel_types::Request,
}

enum ReturnStatus {
    Done,
    AcceptNextInput,
}

fn handle_message(
    message_queue: &mut VecDeque<(QueueMessage, Option<kernel_types::Payload>)>,
    our_name: &str,
) -> anyhow::Result<ReturnStatus> {
    let Ok((source, message)) = bindings::receive() else {
        return Err(anyhow::anyhow!("sequentialize: receive() failed"));
    };

    match message {
        Message::Request(Request { ipc, .. }) => {
            if our_name != source.node {
                return Err(anyhow::anyhow!(
                    "rejecting foreign Message from {:?}",
                    source,
                ));
            }
            let Some(json) = ipc else {
                return Err(anyhow::anyhow!(
                    "rejecting Message without IPC payload from {:?}",
                    source,
                ));
            };
            match serde_json::from_str::<SequentializeRequest>(&json)? {
                SequentializeRequest::QueueMessage(message) => {
                    let payload = bindings::get_payload();
                    message_queue.push_back((message, kernel_types::de_wit_payload(payload)));
                    Ok(ReturnStatus::AcceptNextInput)
                }
                SequentializeRequest::RunQueue => {
                    for item in message_queue.drain(..) {
                        let _ = bindings::send_request(
                            &Address {
                                node: our_name.into(),
                                process: kernel_types::en_wit_process_id(item.0.target),
                            },
                            &kernel_types::en_wit_request(item.0.request),
                            None,
                            Some(&kernel_types::en_wit_payload(item.1).unwrap()),
                        );
                    }
                    Ok(ReturnStatus::Done)
                }
            }
        }
        Message::Response(_) => {
            // we don't care about these
            Ok(ReturnStatus::AcceptNextInput)
        }
    }
}

impl Guest for Component {
    fn init(our: Address) {
        bindings::print_to_terminal(1, "sequentialize: begin");

        let mut message_queue: VecDeque<(QueueMessage, Option<kernel_types::Payload>)> = VecDeque::new();

        loop {
            match handle_message(&mut message_queue, &our.node) {
                Ok(return_status) => match return_status {
                    ReturnStatus::Done => return,
                    ReturnStatus::AcceptNextInput => {}
                },
                Err(e) => {
                    bindings::print_to_terminal(
                        0,
                        format!("sequentialize: error: {:?}", e).as_str(),
                    );
                    panic!("sequentialize: error: {:?}", e);
                }
            };
        }
    }
}
