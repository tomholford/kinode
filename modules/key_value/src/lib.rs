cargo_component_bindings::generate!();

use std::collections::HashMap;

use redb::ReadableTable;
use serde::{Deserialize, Serialize};

use bindings::component::uq_process::types::*;
use bindings::{get_payload, Guest, print_to_terminal, receive, send_response};

mod kernel_types;
use kernel_types as kt;
mod process_lib;

struct Component;

const TABLE: redb::TableDefinition<&[u8], &[u8]> = redb::TableDefinition::new("process");

#[derive(Clone, Debug, Serialize, Deserialize)]
enum KeyValueRequest {
    Write { key: Vec<u8> },
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

fn get_or_make_db<'a>(
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
    print_to_terminal(0, "key_value: before create()");
    let db =  redb::Database::create(format!(
         "{}.redb",
         process_id_string,
     ))?;
    print_to_terminal(0, "key_value: after create()");
    dbs.insert(process_id.clone(), db);
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
        Message::Request(Request { inherit: _ , expects_response: _, ipc, metadata: _ }) => {
            match process_lib::parse_message_ipc(ipc)? {
                KeyValueRequest::Write { key } => {
                    let Payload { mime: _, bytes } = get_payload().ok_or(anyhow::anyhow!(""))?;

                    let db = get_or_make_db(
                        kt::de_wit_process_id(source.process),
                        dbs,
                    )?;

                    let write_txn = db.begin_write()?;
                    {
                        let mut table = write_txn.open_table(TABLE)?;
                        table.insert(&key[..], &bytes[..])?;
                    }
                    write_txn.commit()?;

                    send_response(
                        &Response {
                            ipc: Some(serde_json::to_string(&KeyValueResponse::Write { key })?),
                            metadata: None,
                        },
                        None,
                    );
                },
                KeyValueRequest::Read { key } => {
                    let db = get_or_make_db(
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
                    //  TODO: should we send an error on failure?
                    print_to_terminal(0, format!(
                        "key_value: error: {:?}",
                        e,
                    ).as_str());
                },
            };
        }
    }
}
