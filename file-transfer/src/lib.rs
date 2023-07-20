use std::collections::HashMap;
use serde::{Serialize, Deserialize};
use bindings::component::microkernel_process::types::WitProtomessageType;
use bindings::component::microkernel_process::types::WitRequestTypeWithTarget;

struct Component;

#[derive(Debug, Serialize, Deserialize)]
struct FileTransferGetFile {
    target_ship: String,
    uri: String,
    chunk_size: u64,
}
#[derive(Debug, Serialize, Deserialize)]
struct FileTransferStart {
    uri: String,
    chunk_size: u64,
}
//  should this instead use transfer_id?
#[derive(Debug, Serialize, Deserialize)]
struct FileTransferGetPiece {
    uri: String,
    chunk_size: u64,
    piece_number: u32,
}
#[derive(Debug, Serialize, Deserialize)]
enum FileTransferAction {
    GetFile(FileTransferGetFile),
    Start(FileTransferStart),
    Cancel(u64),  //  transfer_id
    GetPiece(FileTransferGetPiece),
}
 
#[derive(Debug, Serialize, Deserialize)]
struct FileTransferFilePiece {
    transfer_id: u64,
    piece_number: u32,
    piece_hash: u64,
}
#[derive(Debug, Serialize, Deserialize)]
enum FileTransferUpdate {
    FilePiece(FileTransferFilePiece),
    Done(u64),  //  transfer_id
}
 
#[derive(Debug, Serialize, Deserialize)]
struct FileTransferMetadata {
    uri: String,
    transfer_id: u64,
    hash: u64,
    chunk_size: u64,
    number_pieces: u32,
    // piece_hashes: Vec<u64>,  //  ?
}
#[derive(Debug, Serialize, Deserialize)]
struct FileTransferPieceReceived {
    transfer_id: u64,
    piece_number: u32,
}
#[derive(Debug, Serialize, Deserialize)]
struct FileTransferError {
    transfer_id: u64,
    error: String,
}
#[derive(Debug, Serialize, Deserialize)]
enum FileTransferResponse {
    Starting(FileTransferMetadata),
    PieceReceived(FileTransferPieceReceived),
    FileReceived(u64),  //  transfer_id
    Error(FileTransferError),
}

struct IsConfirmedPiece {
    piece_hash: u64,
    is_confirmed: bool,
}
struct Downloading {
    metadata: FileTransferMetadata,
    received_pieces: HashMap<u32, u64>,  //  piece_number: piece_hash
}
struct Uploading {
    metadata: FileTransferMetadata,
    sent_pieces: HashMap<u32, IsConfirmedPiece>,
}



impl bindings::MicrokernelProcess for Component {
    fn run_process(_: String, _: String) {
        bindings::print_to_terminal("file_transfer: start");

        //  in progress
        let mut downloads: HashMap<u64, Downloading> = HashMap::new();
        let mut uploads: HashMap<u64, Uploading> = HashMap::new();

        //  TODO
        loop {
            let mut message_stack = bindings::await_next_message();
            let message = message_stack.pop().unwrap();
            let Some(message_from_loop_string) = message.payload.json else {
                panic!("foo")
            };
            let message_from_loop: serde_json::Value =
                serde_json::from_str(&message_from_loop_string).unwrap();
            if let serde_json::Value::String(action) = &message_from_loop["action"] {
                if action == "receive" {
                    messages.received.push(
                        serde_json::to_value(&message_from_loop_string).unwrap()
                    );
                    bindings::print_to_terminal(
                        format!(
                            "hi++: got message {}",
                            message_from_loop_string
                        ).as_str()
                    );
                } else if action == "send" {
                    messages.sent.push(
                        serde_json::to_value(&message_from_loop_string).unwrap()
                    );
                    let serde_json::Value::String(ref target) =
                        message_from_loop["target"] else { panic!("unexpected target") };
                    let serde_json::Value::String(ref contents) =
                        message_from_loop["contents"] else { panic!("unexpected contents") };
                    let payload = serde_json::json!({
                        "action": "receive",
                        "target": target,
                        "contents": contents,
                    });
                    let response = bindings::component::microkernel_process::types::WitPayload {
                        json: Some(payload.to_string()),
                        bytes: None,
                    };
                    bindings::yield_results(
                        vec![
                            bindings::WitProtomessage {
                                protomessage_type: WitProtomessageType::Request(
                                    WitRequestTypeWithTarget {
                                        is_expecting_response: false,
                                        target_ship: target,
                                        target_app: "hi_lus_lus",
                                    }
                                ),
                                payload: &response,
                            },
                        ].as_slice()
                    );
                } else {
                    bindings::print_to_terminal(
                        format!(
                            "hi++: unexpected action (expected either 'send' or 'receive'): {:?}",
                            &message_from_loop["action"],
                        ).as_str()
                    );
                }
            } else {
                bindings::print_to_terminal(
                    format!(
                        "hi++: unexpected action: {:?}",
                        &message_from_loop["action"],
                    ).as_str()
                );
            }
        }
    }
}

bindings::export!(Component);
