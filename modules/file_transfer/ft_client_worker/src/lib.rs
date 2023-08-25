cargo_component_bindings::generate!();

use serde::{Serialize, Deserialize};

use bindings::{MicrokernelProcess, print_to_terminal, receive};
use bindings::component::microkernel_process::types;

mod process_lib;

struct Component;

fn handle_next_message(
    our: &ProcessAddress,
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
                FileTransferRequest::GetFile { target_node, file_hash, chunk_size } => {
                    //  (1): Start transfer with ft_server
                    //  (2): iteratively GetPiece and Append until have acquired whole file
                    //  (3): clean up

                    //  (1)
                    let start_response = process_lib::send_and_await_receive(
                        target_node.clone(),
                        types::ProcessIdentifier::Name("ft_server".into()),
                        Some(FileTransferRequest::Start {
                            file_hash,
                            chunk_size,
                        }),
                        types::OutboundPayloadBytes::None,
                    )?;
                    let metadata = match start_response {
                        Err(e) => Err(format!("couldn't Start transfer frm ft_server: {}", e)),
                        Ok(start_message) => {
                            let start_json = get_json(start_message)?;
                            match process_lib::parse_message_json(Some(start_json))? {
                                FileTransferResponse::Start(metadata) => metadata,
                                _ => Err(anyhow::anyhow!("unexpected Response resulting from Start transfer from ft_server")),
                            }

                            // match response_message {
                            //     types::InboundMessage::Request(_) => Err(anyhow::anyhow!("unexpected Request resulting from Start transfer from ft_server")),
                            //     types::InboundMessage::Response(types::InboundPayload {
                            //         source,
                            //         json,
                            //         bytes: _,
                            //     }) => {
                            //         match process_lib::parse_message_json(json)? {
                            //             FileTransferResponse::Start(metadata) => metadata,
                            //             _ => Err(anyhow::anyhow!("unexpected Response resulting from Start transfer from ft_server")),
                            //         }
                            //     },
                            // }
                        },
                    }?;
                    assert_eq(target_node, source.node);
                    let state = ClientWorkerState {
                        metadata,
                        current_file_hash: None,
                        number_pieces_received: 0,
                    };

                    //  (2)
                    while state.metadata.number_pieces > state.number_pieces_received {
                        //  TODO: circumvent bytes?
                        let get_piece_response = process_lib::send_and_await_receive(
                            source.node.clone(),
                            source.identifier.clone(),
                            Some(&FileTransferRequest::GetPiece {
                                piece_number: state.number_pieces_received,
                            }),
                            types::OutboundPayloadBytes::None,
                        )?;
                        let bytes = process_lib::get_bytes(get_piece_response)?;

                        let append_response = process_lib::send_and_await_receive(
                            our.node.clone(),
                            types::ProcessIdentifier::Name("lfs".into()),
                            Some(&FsAction::Append(state.current_file_hash)),
                            types::OutboundPayloadBytes::None,
                        )?;
                        let append_json = process_lib::get_json(append_response)?;
                        state.current_file_hash = match process_lib::parse_message_json(Some(append_json))? {
                            FsResponse::Append(file_hash) => file_hash,
                            _ => Err(anyhow::anyhow!("unexpected Response resulting from lfs Append")),
                        };

                        state.number_pieces_received += 1;
                    }

                    //  (3)
                    assert_eq(state.metadata.key.file_hash, state.current_file_hash);
                    print_to_terminal(0, &format!(
                        "file_transfer: successfully downloaded {} from {}",
                        state.metadata.key.file_hash,
                        state.metadata.key.server,
                    ));
                    process_lib::send_one_request(
                        false,
                        &get_file.target_ship,
                        &state.metadata.key.server,
                        Some(FileTransferRequest::Done),
                        types::OutboundPayloadBytes::None,
                        None::<FileTransferContext>,
                    )?;
                    Ok(MessageHandledStatus::Done)
                },
                _ => Err(anyhow::anyhow!("unexpected Request")),
            }
        }
    }
}

impl bindings::MicrokernelProcess for Component {
    fn run_process(our_name: String, process_name: String) {
        print_to_terminal(1, "ft_client_worker: begin");

        match handle_next_message(
            &our,
        ) {
            Ok(_) => { return; },
            Err(e) => {
                print_to_terminal(0, &format!("ft_client_worker: error: {:?}", e));
                panic!();
            },
        };
    }
}
