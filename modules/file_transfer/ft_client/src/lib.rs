cargo_component_bindings::generate!();

use bindings::{MicrokernelProcess, print_to_terminal, receive};
use bindings::component::microkernel_process::types;

mod ft_types;
mod process_lib;

struct Component;

fn handle_next_message(
    our: &types::ProcessAddress,
    //state:,
) -> anyhow::Result<()> {
    let (message, _context) = receive()?;

    match message {
        types::InboundMessage::Response(_) => Err(anyhow::anyhow!("unexpected Response")),
        types::InboundMessage::Request(types::InboundRequest {
            is_expecting_response: _,
            payload: types::InboundPayload {
                source: _,
                json,
                bytes: _,
            },
        }) => {
            match process_lib::parse_message_json(json)? {
                //  TODO: maintain & persist state about ongoing transfers
                //        resume rather than starting from scratch when appropriate
                ft_types::FileTransferRequest::GetFile { target_node, file_hash, chunk_size } => {
                    //  (1) spin up ft_client_worker to handle upload
                    //  (2) send GetFile to client_worker to begin download

                    //  (1)
                    let response = process_lib::send_and_await_receive(
                        our.node.clone(),
                        types::ProcessIdentifier::Name("process_manager".into()),
                        Some(ft_types::ProcessManagerCommand::Start {
                            name: None,
                            wasm_bytes_uri: "fs://sequentialize/file_transfer/ft_client_worker.wasm".into(),  //  TODO; should this be persisted when it becomes a file hash?
                            send_on_panic: ft_types::SendOnPanic::None,
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
                        Err(e) => Err(anyhow::anyhow!("couldn't Start ft_client_worker: {}", e)),
                        Ok(response_message) => {
                            let response_json = process_lib::get_json(&response_message)?;
                            match process_lib::parse_message_json(Some(response_json))? {
                                ft_types::ProcessManagerResponse::Start { id, name: _ } => Ok(id),
                                _ => Err(anyhow::anyhow!("unexpected Response resulting from Start ft_client_worker")),
                            }
                        },
                    }?;

                    //  (2)
                    process_lib::send_one_request(
                        false,
                        &our.node,
                        types::ProcessIdentifier::Id(id),
                        Some(ft_types::FileTransferRequest::GetFile {
                            target_node,
                            file_hash,
                            chunk_size,
                        }),
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
        print_to_terminal(1, "ft_client: begin");

        //  TODO: map? what is key?
        //let mut state: Option<ClientState> = None;

        loop {
            match handle_next_message(
                &our,
                //&mut state,
            ) {
                Ok(_) => {},
                Err(e) => {
                    //  TODO: should bail?
                    print_to_terminal(0, &format!("ft_client: error: {:?}", e));
                },
            };
        }
    }
}
