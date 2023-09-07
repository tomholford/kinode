cargo_component_bindings::generate!();

use std::collections::HashMap;

use redb::ReadableTable;
use serde::{Deserialize, Serialize};

use bindings::component::uq_process::types::*;
use bindings::{Guest, print_to_terminal, receive, send_response};

mod kernel_types;
use kernel_types as kt;
mod process_lib;

struct Component;

const TABLE: redb::TableDefinition<&[u8], &[u8]> = redb::TableDefinition::new("process");

#[derive(Clone, Debug, Serialize, Deserialize)]
enum KeyValueRequest {
    // Initialize,
    Write { key: Vec<u8>, val: Vec<u8> },
    // Write { key: Vec<u8> },
    Read { key: Vec<u8> },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
enum KeyValueResponse {
    Write { key: Vec<u8> },
    Read { key: Vec<u8> },
}

fn process_id_to_string(process_id: &kt::ProcessId) -> String {
    match process_id {
        kt::ProcessId::Id(id) => { format!("{}", id) },
        kt::ProcessId::Name(name) => { name.into() },
    }
}

//  TODO: have persistence
// fn persist_state(
//     our_name: &str,
//     dbs: &HashMap<ProcessIdentifier, redb::Database>,
// ) -> anyhow::Result<Result<types::InboundMessage, types::UqbarError>> {
//     print_to_terminal(1, "kev_value: persist pm state");
//     process_lib::send_and_await_receive(
//         our_name.into(),
//         types::ProcessIdentifier::Name("process_manager".into()),
//         Some(ProcessManagerCommand::PersistState),
//         types::OutboundPayloadBytes::Circumvent(bincode::serialize(
//             &dbs.keys().cloned().collect::<Vec<ProcessIdentifier>>()
//         )?),
//     )
// }

fn get_or_make_db<'a>(
    our_name: &'a str,
    process_id: kt::ProcessId,
    dbs: &'a mut HashMap<kt::ProcessId, redb::Database>,
) -> anyhow::Result<&'a mut redb::Database> {
    if dbs.contains_key(&process_id) {
        let Some(db) = dbs.get_mut(&process_id) else {
            panic!("");
        };
        return Ok(db);
    }
    let process_id_string = process_id_to_string(&process_id);
    let db =  redb::Database::create(format!(
         "{}.redb",
         process_id_string,
     ))?;
    dbs.insert(process_id.clone(), db);
    // persist_state(our_name, &dbs)??;  // TODO
    Ok(dbs.get_mut(&process_id).unwrap())
}

fn handle_message (
    our: &Address,
    dbs: &mut HashMap<kt::ProcessId, redb::Database>,
) -> anyhow::Result<()> {
    let (source, message) = receive().unwrap();
    // let (source, message) = receive()?;

    if our.node != source.node {
        return Err(anyhow::anyhow!(
            "rejecting foreign Message from {:?}",
            source,
        ));
    }

    match message {
        Message::Response(_) => { panic!() },
        Message::Request(Request { inherit, expects_response, ipc, metadata }) => {
            match process_lib::parse_message_ipc(ipc)? {
                // KeyValueRequest::Initialize => {
                //     match bytes {
                //         types::InboundPayloadBytes::Some(bytes) => {
                //             let process_identifiers: Vec<ProcessIdentifier> =
                //                 bincode::deserialize(&bytes[..])?;
                //             for process_identifier in process_identifiers.iter() {
                //                 let process_identifier_string = process_identifier_to_string(
                //                     process_identifier,
                //                 );
                //                 dbs.insert(
                //                     process_identifier.clone(),
                //                     redb::Database::create(format!(
                //                         "{}.redb",
                //                         process_identifier_string,
                //                     ))?,
                //                 );
                //             }
                //         },
                //         _ => {},
                //     }
                //     let _ = process_lib::send_response(
                //         None::<KeyValueResponse>,  //  TODO
                //         types::OutboundPayloadBytes::None,
                //         None::<String>,  //  TODO
                //     )?;
                // },
                // KeyValueRequest::Write { key } => {
                KeyValueRequest::Write { key, val } => {
                    // let types::InboundPayloadBytes::Some(bytes) = bytes else {
                    //     panic!("key_value: no bytes to write");
                    // };

                    let db = get_or_make_db(
                        &our.node,
                        kt::de_wit_process_id(source.process),
                        dbs,
                    )?;

                    let write_txn = db.begin_write()?;
                    {
                        let mut table = write_txn.open_table(TABLE)?;
                        // table.insert(&key[..], &bytes[..])?;
                        table.insert(&key[..], &val[..])?;
                    }
                    write_txn.commit()?;

                    send_response(
                        &Response {
                            ipc: Some(serde_json::to_string(&KeyValueResponse::Write { key })?),
                            metadata: None,
                        },
                        None,
                        // Some(KeyValueResponse::Write { key }),
                        // types::OutboundPayloadBytes::None,
                        // None::<String>,  //  TODO
                    );
                },
                KeyValueRequest::Read { key } => {
                    let db = get_or_make_db(
                        &our.node,
                        kt::de_wit_process_id(source.process),
                        dbs,
                    )?;


                    let read_txn = db.begin_read()?;

                    let table = read_txn.open_table(TABLE)?;

                    match table.get(&key[..])? {
                        None => {
                            send_response(
                                &Response {
                                    ipc: Some(serde_json::to_string(&KeyValueResponse::Read {
                                        key: key.clone(),
                                    })?),
                                    metadata: None,
                                },
                                None,
                            );
                        },
                        Some(v) => {
                            send_response(
                                &Response {
                                    ipc: Some(serde_json::to_string(&KeyValueResponse::Read {
                                        key: key.clone(),
                                    })?),
                                    metadata: None,
                                },
                                Some(&Payload {
                                    mime: None,
                                    bytes: v.value().to_vec(),
                                }),
                            );
                        },
                    };
                },
            }

            Ok(())
        },
    }
}

impl Guest for Component {
    fn init(our: Address) {
        print_to_terminal(1, "key_value: begin");

        let mut dbs: HashMap<kt::ProcessId, redb::Database> = HashMap::new();

        loop {
            match handle_message(&our, &mut dbs) {
                Ok(()) => {},
                Err(e) => {
                    print_to_terminal(0, format!(
                        "key_value: error: {:?}",
                        e,
                    ).as_str());
                },
            };
        }
    }
}
