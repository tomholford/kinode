use bindings::component::microkernel_process::types;

pub fn make_response<'a>(
    payload: &'a types::WitPayload,
    // json_string: Option<String>,
    // bytes: Option<Vec<u8>>,
    context: &'a str,  //  ?
) -> (types::WitProtomessage<'a>, &'a str) {
    (
        types::WitProtomessage {
            protomessage_type: types::WitProtomessageType::Response,
            payload,
        },
        context,
    )
}

pub fn make_request<'a>(
    is_expecting_response: bool,
    target_node: &'a str,
    target_process: &'a str,
    payload: &'a types::WitPayload,
    // json_string: Option<String>,
    // bytes: Option<Vec<u8>>,
    context: &'a str,
) -> (types::WitProtomessage<'a>, &'a str) {
    (
        types::WitProtomessage {
            protomessage_type: types::WitProtomessageType::Request(
                types::WitRequestTypeWithTarget {
                    is_expecting_response,
                    target_ship: target_node,
                    target_app: target_process,
                },
            ),
            payload,
        },
        context,
    )
}

pub fn make_payload(
    json_string: Option<String>,
    bytes: Option<Vec<u8>>,
) -> types::WitPayload {
    types::WitPayload {
        json: json_string,
        bytes,
    }
}

pub fn parse_message_json<T>(json_string: Option<String>) -> Result<T, anyhow::Error>
where for<'a> T: serde::Deserialize<'a> {
    let parsed: T = serde_json::from_str(
        json_string.ok_or(anyhow::anyhow!("json payload empty"))?
                   .as_str()
    )?;
    Ok(parsed)
}
