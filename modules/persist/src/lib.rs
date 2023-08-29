cargo_component_bindings::generate!();

use serde::{Serialize, Deserialize};

use bindings::print_to_terminal;
use bindings::component::microkernel_process::types;

mod process_lib;

struct Component;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessReference {
    pub node: String,
    pub identifier: ProcessIdentifier,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProcessIdentifier {
    Id(u64),
    Name(String),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Payload {
    json: Option<serde_json::Value>,
    bytes: Option<Vec<u8>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestOnPanic {
    pub target: ProcessReference,
    pub payload: Payload,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SendOnPanic {
    None,
    Restart,
    Requests(Vec<RequestOnPanic>),
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProcessManagerCommand {
    Initialize { jwt_secret_bytes: Option<Vec<u8>> },
    Start { process_name: String, wasm_bytes_uri: String, send_on_panic: SendOnPanic },
    Stop { process_name: String },
    Restart { process_name: String },
    ListRunningProcesses,
    PersistState,
}
#[derive(Debug, Serialize, Deserialize)]
pub enum ProcessManagerResponse {
    Initialize,
    ListRunningProcesses { processes: Vec<String> },
    PersistState([u8; 32]),
}

#[derive(Debug, Serialize, Deserialize)]
enum PersistRequest {
    Initialize,
    Get,
    Set { new_value: u64 },
}

#[derive(Debug, Serialize, Deserialize)]
struct State {
    val: Option<u64>,
}

fn persist_state(our_name: &str, state: &State) -> anyhow::Result<types::WitMessage> {
    print_to_terminal(1, "process_manager: persist pm state");
    process_lib::send_request_and_await_response(
        our_name.into(),
        "process_manager".into(),
        Some(ProcessManagerCommand::PersistState),
        types::WitPayloadBytes {
            circumvent: types::WitCircumvent::Send,
            content: Some(bincode::serialize(state)?),
        },
    )
}

fn handle_message(
    state: &mut State,
    our_name: &str,
    // process_name: &str,
) -> anyhow::Result<()> {
    let (message, _context) = bindings::await_next_message()?;
    match message.content.message_type {
        types::WitMessageType::Request(_is_expecting_response) => {
            match process_lib::parse_message_json(message.content.payload.json)? {
                PersistRequest::Initialize => {
                    match message.content.payload.bytes.content {
                        None => {},
                        Some(bytes) => {
                            state.val = bincode::deserialize(&bytes[..])?;
                        },
                    }
                    let _ = process_lib::send_response(
                        None::<State>,
                        types::WitPayloadBytes {
                            circumvent: types::WitCircumvent::False,
                            content: None,
                        },
                        None::<State>,
                    )?;
                },
                PersistRequest::Get => {
                    print_to_terminal(
                        0,
                        format!("persist: state: {:?}", state).as_str(),
                    );
                },
                PersistRequest::Set { new_value } => {
                    state.val = Some(new_value);
                    let _ = persist_state(our_name, state);
                },
            }
        }
        types::WitMessageType::Response => {},
    }
    Ok(())
}

impl bindings::MicrokernelProcess for Component {
    fn run_process(our: types::WitProcessAddress) {
    // fn run_process(our_name: String, process_name: String) {
        print_to_terminal(1, "persist: begin");

        let mut state = State { val: None };
        loop {
            match handle_message(
                &mut state,
                &our.node,
                // &process_name,
            ) {
                Ok(()) => {},
                Err(e) => {
                    print_to_terminal(0, format!("persist: error: {:?}", e).as_str());
                },
            };
        }
    }
}
