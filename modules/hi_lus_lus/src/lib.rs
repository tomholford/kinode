cargo_component_bindings::generate!();

use bindings::component::uq_process::types::*;
use bindings::{print_to_terminal, receive, send_request, UqProcess};
use serde::{Deserialize, Serialize};

struct Component;

struct Messages {
    received: Vec<HiMessage>,
    sent: Vec<HiMessage>,
}

#[derive(Clone, Serialize, Deserialize)]
struct HiMessage {
    to: String,
    from: String,
    contents: String,
}

impl UqProcess for Component {
    fn init(our: Address) {
        print_to_terminal(1, "hi++: start");

        let mut messages = Messages {
            received: vec![],
            sent: vec![],
        };

        loop {
            let Ok((source, message)) = receive() else {
                print_to_terminal(0, "hi++: got network error");
                continue;
            };
            let Message::Request(request) = message else {
                print_to_terminal(0, "hi++: got unexpected message");
                continue;
            };
            let Ok(msg) = serde_json::from_str::<HiMessage>(&request.ipc.unwrap_or_default()) else {
                print_to_terminal(0, "hi++: got invalid message");
                continue;
            };
            if msg.to != our.node && source.node == our.node {
                messages.sent.push(msg.clone());
                send_request(
                    &Address {
                        node: msg.to.clone(),
                        process: ProcessId::Name("hi++".to_string()),
                    },
                    &Request {
                        inherit: true,
                        expects_response: false,
                        ipc: Some(serde_json::to_string(&msg).unwrap()),
                        metadata: None,
                    },
                    None,
                    None,
                );
                continue;
            } else if msg.to == our.node {
                messages.received.push(msg.clone());
                print_to_terminal(0, &format!("{}: {}", msg.from, msg.contents));
                continue;
            }
        }
    }
}
