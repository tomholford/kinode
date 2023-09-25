cargo_component_bindings::generate!();

use bindings::component::uq_process::types::*;
use bindings::{print_to_terminal, receive, send_requests, Guest};
use serde_json::{from_str, to_string, Value};

struct Component;

/*
 *  sends a bunch of empty bytes across network
 *  each chunk is "size" field bytes large
 *  format: !message our net_tester {"chunks": 1, "size": 65536, "target": "tester3"}
 */
impl Guest for Component {
    fn init(our: Address) {
        print_to_terminal(0, "net_tester: init");
        loop {
            let (source, message) = match receive() {
                Ok((source, message)) => (source, message),
                Err((e, _context)) => {
                    print_to_terminal(0, &format!("net_tester: got network error: {:?}", e.kind));
                    continue;
                }
            };
            let Message::Request(request) = message else {
                print_to_terminal(0, "net_tester: got unexpected non-Request");
                continue;
            };
            if source.node != our.node {
                print_to_terminal(
                    0,
                    &format!(
                        "net_tester: got message #{} from {}",
                        request.ipc.unwrap_or_default(),
                        source.node,
                    ),
                );
                continue;
            } else if let ProcessId::Name(name) = source.process {
                if name != "terminal" {
                    continue;
                }
                let command: Value = from_str(&request.ipc.unwrap_or_default()).unwrap();
                // read size of transfer to test and do it
                let chunks: u64 = command["chunks"].as_u64().unwrap();
                let chunk: Vec<u8> = vec![0xfu8; command["size"].as_u64().unwrap() as usize];
                let target = command["target"].as_str().unwrap();

                let mut messages =
                    Vec::<(Address, Request, Option<Context>, Option<Payload>)>::new();
                for num in 1..chunks + 1 {
                    messages.push((
                        Address {
                            node: target.into(),
                            process: ProcessId::Name("net_tester".into()),
                        },
                        Request {
                            inherit: false,
                            expects_response: None,
                            ipc: Some(num.to_string()),
                            metadata: None,
                        },
                        None,
                        Some(Payload {
                            mime: None,
                            bytes: chunk.clone(),
                        }),
                    ));
                }
                send_requests(&messages);
                continue;
            }
        }
    }
}
