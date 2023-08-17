//  TODO: rewrite lib, given new bindgen behavior

use super::bindings::component::microkernel_process::types;

pub fn make_request<T>(
    is_expecting_response: bool,
    node: &str,
    process: &str,
    payload: types::WitPayload,
    context: Option<T>,
) -> anyhow::Result<(Vec<types::WitProtorequest>, String)>
where
    T: serde::Serialize
{
    Ok((
        vec![types::WitProtorequest {
            is_expecting_response,
            target: types::WitProcessNode {
                node: node.into(),
                process: process.into(),
            },
            payload,
        }],
        match context {
            None => "".into(),
            Some(c) => serde_json::to_string(&c)?,
        },
    ))
}

pub fn make_payload<T>(
    json_struct: Option<T>,
    bytes: Option<Vec<u8>>,
) -> anyhow::Result<types::WitPayload>
where
     T: serde::Serialize
{
    Ok(types::WitPayload {
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

pub fn send_one_request<T, U>(
    is_expecting_response: bool,
    target_node: &str,
    target_process: &str,
    json_struct: Option<T>,
    bytes: Option<Vec<u8>>,
    context: Option<U>,
) -> anyhow::Result<()>
where
     T: serde::Serialize,
     U: serde::Serialize,
{
    let payload = make_payload(json_struct, bytes)?;
    let protorequest = vec![types::WitProtorequest {
        is_expecting_response,
        target: types::WitProcessNode {
            node: target_node.into(),
            process: target_process.into(),
        },
        payload,
    }];
    let context = match context {
        None => "".into(),
        Some(c) => serde_json::to_string(&c)?,
    };
    let request = (
        protorequest.as_slice(),
        context.as_str(),
    );
    super::bindings::send_requests(Ok(request));

    Ok(())
}

pub fn send_response<T, U>(
    json_struct: Option<T>,
    bytes: Option<Vec<u8>>,
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
        &payload,
        context.as_str(),
    );
    super::bindings::send_response(Ok(response));

    Ok(())
}

pub fn send_request_and_await_response<T>(
    target_node: String,
    target_process: String,
    json_struct: Option<T>,
    bytes: Option<Vec<u8>>,
) -> anyhow::Result<types::WitMessage>
where
     T: serde::Serialize,
{
    let payload = make_payload(json_struct, bytes)?;
    match super::bindings::send_request_and_await_response(
        &types::WitProcessNode {
            node: target_node,
            process: target_process,
        },
        &payload,
    ) {
        Ok(r) => Ok(r),
        Err(e) => Err(anyhow::anyhow!("{}", e)),
    }
}
