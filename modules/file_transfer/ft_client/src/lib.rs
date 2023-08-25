cargo_component_bindings::generate!();

use serde::{Serialize, Deserialize};

use bindings::{MicrokernelProcess, print_to_terminal, receive};
use bindings::component::microkernel_process::types;

mod process_lib;

struct Component;

fn handle_next_message(
    our: &ProcessAddress,
    //state:,
) -> anyhow::Result<MessageHandledStatus> {
    let (message, _context) = receive()?;

    match message {
        types::InboundMessage::Response(_) => Err(anyhow::anyhow!("unexpected Response")),
        types::InboundMessage::Request(types::InboundRequest {
            is_expecting_response,
            payload: types::InboundPayload {
                source,
                json,
                bytes,
            },
        }) => {
            match process_lib::parse_message_json(json)? {
                //  TODO: maintain & persist state about ongoing transfers
                //        resume rather than starting from scratch when appropriate
                FileTransferRequest::GetFile { target_node, file_hash, chunk_size } => {
                    //  (1) spin up ft_client_worker to handle upload
                    //  (2) send GetFile to client_worker to begin download

                    //  (1)
                    let response = process_lib::send_and_await_receive(
                        our.node.clone(),
                        types::ProcessIdentifier::Name("process_manager".into()),
                        Some(ProcessManagerCommand::Start {
                            name: None,
                            wasm_bytes_uri: "fs://sequentialize/file_transfer/ft_client_worker.wasm",  //  TODO; should this be persisted when it becomes a file hash?
                            send_on_panic: SendOnPanic::None,
                            //  TODO: inform client and/or server_worker?
                            // send_on_panic: SendOnPanic::Requests(vec![
                            //     RequestOnPanic {
                            //         target: ProcessReference {
                            //         },
                            //         json: ,
                            //         bytes: TransitPayloadBytes::None,
                            //     },
                            // ]),
                        }),
                        types::OutboundPayloadBytes::None,
                    )?;
                    let id = match response {
                        Err(e) => Err(format!("couldn't Start ft_client_worker: {}", e)),
                        Ok(response_message) => {
                            match response_message {
                                types::InboundMessage::Request(_) => Err(anyhow::anyhow!("unexpected Request resulting from Start ft_client_worker")),
                                types::InboundMessage::Response(types::InboundPayload {
                                    source: _,
                                    json,
                                    bytes: _,
                                }) => {
                                    match process_lib::parse_message_json(json)? {
                                        ProcessManagerResponse::Start { id, name: _ } => id,
                                        _ => Err(anyhow::anyhow!("unexpected Response resulting from Start ft_client_worker")),
                                    }
                                },
                            }
                        },
                    }?;

                    //  (2)
                    process_lib::send_one_request(
                        false,
                        &our.node,
                        types::ProcessIdentifier::Id(id),
                        Some(FileTransferRequest::GetFile {
                            target_node,
                            file_hash,
                            chunk_size,
                        }),
                        types::OutboundPayloadBytes::None,
                        None,
                    )?;
                },
                _ => Err(anyhow::anyhow!("unexpected Request")),
            }
        }
}

impl bindings::MicrokernelProcess for Component {
    fn run_process(our: ProcessAddress) {
        print_to_terminal(1, "ft_client: begin");

        //  TODO: map? what is key?
        //let mut state: Option<ClientState> = None;

        loop {
            match handle_next_message(
                &our,
                //&mut state,
            ) {
                Ok(status) => {
                    match status {
                        MessageHandledStatus::ReadyForNext => {},
                        MessageHandledStatus::Done => {
                            return;
                        },
                    }
                },
                Err(e) => {
                    //  TODO: should bail?
                    print_to_terminal(0, &format!("ft_client: error: {:?}", e));
                },
            };
        }
    }
}
