cargo_component_bindings::generate!();

use bindings::{MicrokernelProcess, print_to_terminal, receive};
use bindings::component::microkernel_process::types;

mod ft_types;
mod process_lib;

struct Component;

fn handle_next_message(
    our: &types::ProcessAddress,
) -> anyhow::Result<()> {
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
                ft_types::FileTransferRequest::GetFile { target_node, file_hash, chunk_size } => {
                    //  (1): Start transfer with ft_server
                    //  (2): iteratively GetPiece and Append until have acquired whole file
                    //  (3): clean up

                    //  (1)
                    let start_response = process_lib::send_and_await_receive(
                        target_node.clone(),
                        types::ProcessIdentifier::Name("ft_server".into()),
                        Some(ft_types::FileTransferRequest::Start {
                            file_hash,
                            chunk_size,
                        }),
                        types::OutboundPayloadBytes::None,
                    )?;
                    let metadata = match start_response {
                        Err(e) => Err(anyhow::anyhow!("couldn't Start transfer frm ft_server: {}", e)),
                        Ok(start_message) => {
                            let start_json = process_lib::get_json(&start_message)?;
                            match process_lib::parse_message_json(Some(start_json))? {
                                ft_types::FileTransferResponse::Start(metadata) => Ok(metadata),
                                _ => Err(anyhow::anyhow!("unexpected Response resulting from Start transfer from ft_server")),
                            }
                        },
                    }?;
                    assert_eq!(target_node, source.node);
                    let state = ft_types::ClientWorkerState {
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
                            Some(&ft_types::FileTransferRequest::GetPiece {
                                piece_number: state.number_pieces_received,
                            }),
                            types::OutboundPayloadBytes::None,
                        )?;
                        let bytes = process_lib::get_bytes(
                            match get_piece_response{
                                Err(e) => Err(anyhow::anyhow!("unexpected Response resulting from Start transfer from ft_server")),
                                Ok(gpr) => Ok(gpr),
                            }?
                        )?;

                        let append_response = process_lib::send_and_await_receive(
                            our.node.clone(),
                            types::ProcessIdentifier::Name("lfs".into()),
                            Some(&ft_types::FsAction::Append(state.current_file_hash)), // lfs interface will reflect this
                            types::OutboundPayloadBytes::None,
                        )?;
                        state.current_file_hash = match append_response {
                            Err(e) => Err(anyhow::anyhow!("couldn't Append file piece: {}", e)),
                            Ok(append_message) => {
                                let append_json = process_lib::get_json(&append_message)?;
                                match process_lib::parse_message_json(Some(append_json))? {
                                    ft_types::FsResponse::Append(file_hash) => Ok(Some(file_hash)),
                                    _ => Err(anyhow::anyhow!("unexpected Response resulting from lfs Append")),
                                }
                            },
                        }?;

                        state.number_pieces_received += 1;
                    }

                    //  (3)
                    assert_eq!(Some(state.metadata.key.file_hash), state.current_file_hash);
                    print_to_terminal(0, &format!(
                        "file_transfer: successfully downloaded {:?} from {}",
                        state.metadata.key.file_hash,
                        state.metadata.key.server,
                    ));
                    process_lib::send_one_request(
                        false,
                        &state.metadata.key.server,
                        source.identifier.clone(),
                        Some(ft_types::FileTransferRequest::Done),
                        types::OutboundPayloadBytes::None,
                        None::<ft_types::FileTransferContext>,
                    )?;
                    Ok(())
                },
                _ => Err(anyhow::anyhow!("unexpected Request")),
            }
        }
    }
}

impl MicrokernelProcess for Component {
    fn run_process(our: types::ProcessAddress) {
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
