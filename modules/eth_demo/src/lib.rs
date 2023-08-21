cargo_component_bindings::generate!();

use bindings::component::microkernel_process::types;
use serde_json::json;
use alloy_sol_types::{sol, SolEnum, SolType, SolCall};

mod process_lib;

struct Component;

sol! {
    // Required functions
    function totalSupply() external view returns (uint256);
    function balanceOf(address account) external view returns (uint256);
    function transfer(address recipient, uint256 amount) external returns (bool);
    function allowance(address owner, address spender) external view returns (uint256);
    function approve(address spender, uint256 amount) external returns (bool);
    function transferFrom(address sender, address recipient, uint256 amount) external returns (bool);

    // Required events
    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);
}

impl bindings::MicrokernelProcess for Component {
    fn run_process(our: String, dap: String) {
        bindings::print_to_terminal(1, "eth-demo: start");

        loop {
            let (message, _) = bindings::await_next_message().unwrap();  //  TODO: handle error properly
            let Some(message_from_loop_string) = message.content.payload.json else {
                panic!("eth demo requires json payload")
            };
            let message_from_loop: serde_json::Value =
                serde_json::from_str(&message_from_loop_string).unwrap();
            
            // let asdf = process_lib::send_request_and_await_response(
            //     our.clone(),
            //     "eth_rpc".to_string(),
            //     Some(json!({"method":"foo", "params":"bar"})),
            //     types::WitPayloadBytes {
            //         circumvent: types::WitCircumvent::False,
            //         content: None,
            //     },
            // );
            let call = totalSupplyCall{};
            let call_data = call.encode();
            let meme = call_data.iter().map(|byte| format!("{:02x}", byte)).collect::<String>();
            bindings::print_to_terminal(0, format!("asdf {}", meme).as_str());
        }
    }
}
