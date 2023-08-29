cargo_component_bindings::generate!();

use bindings::{MicrokernelProcess, print_to_terminal, receive};
use bindings::component::microkernel_process::types;

mod ft_types;
mod process_lib;

struct Component;

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

fn handle_next_message(our: &types::ProcessAddress) -> anyhow::Result<()> {
    let (message, _context) = receive()?;
    match message {
        types::InboundMessage::Response(_) => Err(anyhow::anyhow!("unexpected Response")),
        types::InboundMessage::Request(types::InboundRequest {
            is_expecting_response: _,
            payload: types::InboundPayload {
                source,
                json,
                bytes: _,
            },
        }) => {
            match process_lib::parse_message_json(json)? {
                ft_types::FileTransferRequest::Start { file_hash, chunk_size } => {
                    //  (1) spin up ft_server_worker to handle upload
                    //  (2) send StartWorker to server_worker to begin upload

                    //  (1)
                    let response = process_lib::send_and_await_receive(
                        our.node.clone(),
                        types::ProcessIdentifier::Name("process_manager".into()),
                        Some(ft_types::ProcessManagerCommand::Start {
                            name: None,
                            wasm_bytes_uri: "fs://sequentialize/ft_server_worker.wasm".into(),  //  TODO; should this be persisted when it becomes a file hash?
                            send_on_panic: ft_types::SendOnPanic::None,
                            //  TODO: inform server and/or client_worker?
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
                        Err(e) => Err(anyhow::anyhow!("couldn't Start ft_server_worker: {}", e)),
                        Ok(response_message) => {
                            match response_message {
                                types::InboundMessage::Request(_) => Err(anyhow::anyhow!("unexpected Request resulting from Start ft_server_worker")),
                                types::InboundMessage::Response(types::InboundPayload {
                                    source: _,
                                    json,
                                    bytes: _,
                                }) => {
                                    match process_lib::parse_message_json(json)? {
                                        ft_types::ProcessManagerResponse::Start { id, name: _ } => Ok(id),
                                        _ => Err(anyhow::anyhow!("unexpected Response resulting from Start ft_server_worker")),
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
                        Some(ft_types::FileTransferRequest::StartWorker {
                            // client_worker: source,
                            client_worker: de_wit_process_reference(&source),
                            file_hash,
                            chunk_size,
                        }),
                        types::OutboundPayloadBytes::None,
                        None::<String>,
                    )?;

                    Ok(())
                },
                _ => Err(anyhow::anyhow!("unexpected Request")),
            }
        },
    }
}

impl MicrokernelProcess for Component {
    fn run_process(our: types::ProcessAddress) {
        print_to_terminal(1, "ft_server: begin");

        loop {
            match handle_next_message(&our) {
                Ok(_) => {},
                Err(e) => {
                    //  TODO: should bail?
                    print_to_terminal(0, &format!("ft_server: error: {:?}", e));
                },
            };
        }
    }
}
