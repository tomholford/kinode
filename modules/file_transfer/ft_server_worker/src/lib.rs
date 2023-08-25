cargo_component_bindings::generate!();

use serde::{Serialize, Deserialize};

use bindings::{MicrokernelProcess, print_to_terminal, receive};
use bindings::component::microkernel_process::types;

mod process_lib;

struct Component;

fn div_round_up(numerator: u64, denominator: u64) -> u64 {
    (numerator + denominator - 1) / denominator
}

fn handle_next_message(
    our: ProcessAddress,
    state: &mut Option<ServerWorkerState>,
) -> anyhow::Result<MessageHandledStatus> {
    let (message, _context) = receive()?;

    //  if we have been StartWorker'd, state will be set:
    //   then confirm source is expected client_worker
    match state {
        None => {},
        Some(state) => {
            let source = process_lib::get_source(&message);
            assert_eq!(state.client_worker, source);
        },
    }

    match message {
        types::InboundMessage::Response(_) => Err(anyhow::anyhow!("unexpected Response")),
        types::InboundMessage::Request(types::InboundRequest {
            is_expecting_response,
            payload: types::InboundPayload { source, json, bytes },
        }) => {
            match process_lib::parse_message_json(json)? {
                FileTransferRequest::StartWorker { client_worker, file_hash, chunk_size } => {
                    //  (1) get file length
                    //  (2) instantiate metadata & add to state
                    //  (3) inform client_worker we are ready to upload

                    //  (1)
                    let response = process_lib::send_and_await_receive(
                        our.node.clone(),
                        types::ProcessIdentifier::Name("lfs".into()),
                        Some(FsAction::Length(file_hash)),
                        types::OutboundPayloadBytes::None,
                    )?;
                    let length = match response {
                        Err(e) => { panic!() },  //  TODO: pass error on to client_worker
                        Ok(response_message) => {
                            match response_message {
                                types::InboundMessage::Request(_) => Err(anyhow::anyhow!("unexpected Request resulting from Length lfs call")),
                                types::InboundMessage::Response(types::InboundPayload {
                                    source: _,
                                    json,
                                    bytes: _,
                                }) => {
                                    match process_lib::parse_message_json(json)? {
                                        FsResponse::Length(number_bytes) => number_bytes,
                                        _ => Err(anyhow::anyhow!("unexpected Response resulting from Length lfs call")),
                                    }
                                }
                            }
                        },
                    }?;

                    //  (2)
                    let number_pieces = div_round_up(
                        file_metadata.len,
                        chunk_size
                    );
                    let key = FileTransferKey {
                        client: client_worker.node.clone(),
                        server: our.node.clone(),
                        file_hash: file_hash.clone(),
                    };
                    let metadata = FileTransferMetadata {
                        key,
                        chunk_size,
                        number_pieces,
                        number_bytes,
                    };

                    *state = Some(ServerWorkerState {
                        client_worker,
                        metadata: metadata.clone(),
                        number_pieces_sent: 0,
                    });

                    //  (3)
                    process_lib::send_response(
                        Some(FileTransferResponse::Start(metadata)),
                        types::OutboundPayloadBytes::None,
                        None::<FileTransferContext>,
                    )?;
                },
                FileTransferRequest::GetPiece { piece_number } => {
                    //  (1) get chunk bytes from lfs
                    //  (2) send chunk bytes

                    //  (1)
                    let Some(state) = state else { panic!() };
                    let file_hash = state.metadata.key.file_hash.clone();
                    let chunk_size = state.metadata.chunk_size.clone();
                    let start = chunk_size * piece_number;
                    let response = process_lib::send_and_await_receive(
                        our.node.clone(),
                        types::ProcessIdentifier::Name("lfs".into()),
                        Some(FsAction::ReadChunk(ReadChunkRequest {
                            file_hash,
                            start,
                            length: chunk_size,
                        })),
                        types::OutboundPayloadBytes::None,
                    )?;
                    let bytes = match response {
                        Err(e) => { panic!() },  //  TODO: pass error on to client_worker
                        Ok(response_message) => {
                            match response_message {
                                types::InboundMessage::Request(_) => Err(anyhow::anyhow!("unexpected Request resulting from ReadChunk lfs call")),
                                types::InboundMessage::Response(types::InboundPayload {
                                    source: _,
                                    json: _,
                                    bytes,
                                }) => {
                                    let types::InboundPayloadBytes::Some(bytes) = bytes else {
                                        Err(anyhow::anyhow!("Request resulting from ReadChunk lfs call has no bytes"))
                                    };
                                    bytes
                                }
                            }
                        },
                    }?;

                    //  (2)
                    process_lib::send_response(
                        Some(FileTransferResponse::GetPiece { piece_number }),
                        types::OutboundPayloadBytes::Some(bytes),
                        None::<FileTransferContext>,
                    )?;
                },
                FileTransferRequest::Done => {
                    print_to_terminal(0, format!(
                        "file_transfer: done transferring {} to {}",
                        state.metadata.key.file_hash,
                        state.metadata.key.client,
                    ).as_str());

                    Ok(MessageHandledStatus::Done)
                },
                _ => Err(anyhow::anyhow!("unexpected Request")),
            }
        },
    }

    Ok(MessageHandledStatus::ReadyForNext)
}

impl bindings::MicrokernelProcess for Component {
    fn run_process(our: ProcessAddress) {
        print_to_terminal(1, "ft_server_worker: begin");

        let mut state: Option<ServerWorkerState> = None;

        loop {
            match handle_next_message(
                &our,
                &mut state,
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
                    //  TODO: should bail / Cancel
                    print_to_terminal(0, format!("{}: error: {:?}", process_name, e).as_str());
                },
            };
        }
    }
}
