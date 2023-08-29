cargo_component_bindings::generate!();

use bindings::{MicrokernelProcess, print_to_terminal, receive};
use bindings::component::microkernel_process::types;

mod ft_types;
mod process_lib;

struct Component;

fn div_round_up(numerator: u64, denominator: u64) -> u64 {
    (numerator + denominator - 1) / denominator
}

fn de_wit_process_reference(wit: &types::ProcessReference) -> ft_types::ProcessReference {
    ft_types::ProcessReference {
        node: wit.node.clone(),
        identifier: de_wit_process_identifier(&wit.identifier),
    }
}
fn de_wit_process_identifier(wit: &types::ProcessIdentifier) -> ft_types::ProcessIdentifier {
    match wit {
        types::ProcessIdentifier::Id(id) => ft_types::ProcessIdentifier::Id(id.clone()),
        types::ProcessIdentifier::Name(name) => ft_types::ProcessIdentifier::Name(name.clone()),
    }
}

fn handle_next_message(
    our: &types::ProcessAddress,
    state: &mut Option<ft_types::ServerWorkerState>,
) -> anyhow::Result<ft_types::MessageHandledStatus> {
    let (message, _context) = receive()?;

    //  if we have been StartWorker'd, state will be set:
    //   then confirm source is expected client_worker
    match state {
        None => {},
        Some(state) => {
            let source = process_lib::get_source(&message);
            assert_eq!(state.client_worker, de_wit_process_reference(&source));
        },
    }

    match message {
        types::InboundMessage::Response(_) => Err(anyhow::anyhow!("unexpected Response")),
        types::InboundMessage::Request(types::InboundRequest {
            is_expecting_response: _,
            payload: types::InboundPayload { source: _, json, bytes: _ },
        }) => {
            match process_lib::parse_message_json(json)? {
                ft_types::FileTransferRequest::StartWorker { client_worker, file_hash, chunk_size } => {
                    //  (1) get file length
                    //  (2) instantiate metadata & add to state
                    //  (3) inform client_worker we are ready to upload

                    //  (1)
                    let response = process_lib::send_and_await_receive(
                        our.node.clone(),
                        types::ProcessIdentifier::Name("lfs".into()),
                        Some(ft_types::FsAction::Length(file_hash)),
                        types::OutboundPayloadBytes::None,
                    )?;
                    let number_bytes = match response {
                        Err(_e) => { panic!() },  //  TODO: pass error on to client_worker
                        Ok(response_message) => {
                            match response_message {
                                types::InboundMessage::Request(_) => Err(anyhow::anyhow!("unexpected Request resulting from Length lfs call")),
                                types::InboundMessage::Response(types::InboundPayload {
                                    source: _,
                                    json,
                                    bytes: _,
                                }) => {
                                    match process_lib::parse_message_json(json)? {
                                        ft_types::FsResponse::Length(number_bytes) => Ok(number_bytes),
                                        _ => Err(anyhow::anyhow!("unexpected Response resulting from Length lfs call")),
                                    }
                                }
                            }
                        },
                    }?;

                    //  (2)
                    let number_pieces = div_round_up(
                        number_bytes,
                        chunk_size
                    );
                    let key = ft_types::FileTransferKey {
                        client: client_worker.node.clone(),
                        server: our.node.clone(),
                        file_hash: file_hash.clone(),
                    };
                    let metadata = ft_types::FileTransferMetadata {
                        key,
                        chunk_size,
                        number_pieces,
                        number_bytes,
                    };

                    *state = Some(ft_types::ServerWorkerState {
                        client_worker,
                        metadata: metadata.clone(),
                    });

                    //  (3)
                    process_lib::send_response(
                        Some(ft_types::FileTransferResponse::Start(metadata)),
                        types::OutboundPayloadBytes::None,
                        None::<ft_types::FileTransferContext>,
                    )?;

                    Ok(ft_types::MessageHandledStatus::ReadyForNext)
                },
                ft_types::FileTransferRequest::GetPiece { piece_number } => {
                    //  (1) get chunk bytes from lfs
                    //  (2) send chunk bytes

                    //  (1)
                    let Some(state) = state else { panic!() };
                    let file_hash = state.metadata.key.file_hash.clone();
                    let chunk_size = state.metadata.chunk_size.clone();
                    let start = chunk_size * piece_number;
                    let number_bytes_left = state.metadata.number_bytes - start;
                    let length =
                        if number_bytes_left > chunk_size {
                            chunk_size
                        } else {
                            number_bytes_left
                        };
                    let response = process_lib::send_and_await_receive(
                        our.node.clone(),
                        types::ProcessIdentifier::Name("lfs".into()),
                        Some(ft_types::FsAction::ReadChunk(ft_types::ReadChunkRequest {
                            file_hash,
                            start,
                            length,
                        })),
                        types::OutboundPayloadBytes::None,
                    )?;
                    let bytes = match response {
                        Err(_e) => { panic!() },  //  TODO: pass error on to client_worker
                        Ok(response_message) => {
                            match response_message {
                                types::InboundMessage::Request(_) => Err(anyhow::anyhow!("unexpected Request resulting from ReadChunk lfs call")),
                                types::InboundMessage::Response(types::InboundPayload {
                                    source: _,
                                    json: _,
                                    bytes,
                                }) => {
                                    let types::InboundPayloadBytes::Some(bytes) = bytes else {
                                        return Err(anyhow::anyhow!("Request resulting from ReadChunk lfs call has no bytes"));
                                    };
                                    Ok(bytes)
                                }
                            }
                        },
                    }?;

                    //  (2)
                    process_lib::send_response(
                        Some(ft_types::FileTransferResponse::GetPiece { piece_number }),
                        types::OutboundPayloadBytes::Some(bytes),
                        None::<ft_types::FileTransferContext>,
                    )?;

                    Ok(ft_types::MessageHandledStatus::ReadyForNext)
                },
                ft_types::FileTransferRequest::Done => {
                    let Some(state) = state else { panic!() };
                    print_to_terminal(0, format!(
                        "file_transfer: done transferring {:?} to {}",
                        state.metadata.key.file_hash,
                        state.metadata.key.client,
                    ).as_str());

                    Ok(ft_types::MessageHandledStatus::Done)
                },
                _ => Err(anyhow::anyhow!("unexpected Request")),
            }
        },
    }

}

impl MicrokernelProcess for Component {
    fn run_process(our: types::ProcessAddress) {
        print_to_terminal(1, "ft_server_worker: begin");

        let mut state: Option<ft_types::ServerWorkerState> = None;

        loop {
            match handle_next_message(
                &our,
                &mut state,
            ) {
                Ok(status) => {
                    match status {
                        ft_types::MessageHandledStatus::ReadyForNext => {},
                        ft_types::MessageHandledStatus::Done => {
                            return;
                        },
                    }
                },
                Err(e) => {
                    //  TODO: should bail / Cancel
                    print_to_terminal(0, &format!("ft_server_worker: error: {:?}", e));
                },
            };
        }
    }
}
