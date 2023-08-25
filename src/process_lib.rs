//  TODO: rewrite lib, given new bindgen behavior

use super::bindings::component::microkernel_process::types;

pub fn make_request<T, U>(
    is_expecting_response: bool,
    target_node: &str,
    target_process: types::ProcessIdentifier,
    json_struct: Option<T>,
    bytes: types::OutboundPayloadBytes,
    context: Option<U>,
) -> anyhow::Result<types::JoinedRequests>
where
    T: serde::Serialize,
    U: serde::Serialize,
{
    let payload = make_payload(json_struct, bytes)?;
    let request = types::OutboundRequest {
        is_expecting_response,
        target: types::ProcessReference {
            node: target_node.into(),
            identifier: target_process,
        },
        payload,
    };
    let context = match context {
        None => "".into(),
        Some(c) => serde_json::to_string(&c)?,
    };
    let joined_requests = (
        vec![request],
        context,
    );

    Ok(joined_requests)
}

pub fn make_payload<T>(
    json_struct: Option<T>,
    bytes: types::OutboundPayloadBytes,
) -> anyhow::Result<types::OutboundPayload>
where
     T: serde::Serialize
{
    Ok(types::OutboundPayload {
        json: match json_struct {
            None => None,
            Some(j) => Some(serde_json::to_string(&j)?),
        },
        bytes,
    })
}

pub fn parse_message_json<T>(json_string: Option<String>) -> anyhow::Result<T>
where
    for<'a> T: serde::Deserialize<'a>
{
    let parsed: T = serde_json::from_str(
        json_string.ok_or(anyhow::anyhow!("json payload empty"))?
                   .as_str()
    )?;
    Ok(parsed)
}

pub fn make_outbound_bytes_from_noncircumvented_inbound(
    bytes: types::InboundPayloadBytes,
) -> anyhow::Result<types::OutboundPayloadBytes> {
    match bytes {
        types::InboundPayloadBytes::None => Ok(types::OutboundPayloadBytes::None),
        types::InboundPayloadBytes::Some(bytes) => {
            Ok(types::OutboundPayloadBytes::Some(bytes))
        },
        types::InboundPayloadBytes::Circumvented => {
            Err(anyhow::anyhow!("inbound bytes are Circumvented"))
        },
    }
}

pub fn send_one_request<T, U>(
    is_expecting_response: bool,
    target_node: &str,
    target_process: types::ProcessIdentifier,
    json_struct: Option<T>,
    bytes: types::OutboundPayloadBytes,
    context: Option<U>,
) -> anyhow::Result<()>
where
     T: serde::Serialize,
     U: serde::Serialize,
{
    let joined_requests = make_request(
        is_expecting_response,
        target_node,
        target_process,
        json_struct,
        bytes,
        context,
    )?;

    let outbound_message = types::OutboundMessage::Requests(vec![joined_requests]);
    super::bindings::send(Ok(&outbound_message));

    Ok(())
}

pub fn send_response<T, U>(
    json_struct: Option<T>,
    bytes: types::OutboundPayloadBytes,
    context: Option<U>,  //  ?
) -> anyhow::Result<()>
where
     T: serde::Serialize,
     U: serde::Serialize,
{
    let payload = make_payload(json_struct, bytes)?;
    let context = match context {
        None => "".into(),
        Some(c) => serde_json::to_string(&c)?,
    };
    let response = (
        payload,
        context,
    );
    let outbound_message = types::OutboundMessage::Response(response);
    super::bindings::send(Ok(&outbound_message));

    Ok(())
}

pub fn send_and_await_receive<T>(
    target_node: String,
    target_process: types::ProcessIdentifier,
    json_struct: Option<T>,
    bytes: types::OutboundPayloadBytes,
) -> anyhow::Result<Result<types::InboundMessage, types::UqbarError>>
where
     T: serde::Serialize,
{
    let payload = make_payload(json_struct, bytes)?;
    Ok(super::bindings::send_and_await_receive(
        &types::ProcessReference {
            node: target_node,
            identifier: target_process,
        },
        &payload,
    ))
}
