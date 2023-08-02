use bindings::component::microkernel_process::types;

pub fn make_response<'a, T>(
    payload: &'a types::WitPayload,
    context: Option<T>,  //  ?
) -> anyhow::Result<(types::WitProtomessage<'a>, String)>
where
    T: serde::Serialize
{
    Ok((
        types::WitProtomessage {
            protomessage_type: types::WitProtomessageType::Response,
            payload,
        },
        match context {
            None => "".into(),
            Some(c) => serde_json::to_string(&c)?,
        },
    ))
}

pub fn make_request<'a, T>(
    is_expecting_response: bool,
    target_node: &'a str,
    target_process: &'a str,
    payload: &'a types::WitPayload,
    context: Option<T>,
) -> anyhow::Result<(types::WitProtomessage<'a>, String)>
where
    T: serde::Serialize
{
    Ok((
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

pub fn yield_results(results: Vec<(types::WitProtomessage, String)>) {
    let strings = results.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>();

    let mut formatted_results = vec![];
    for (i, result) in results.into_iter().enumerate() {
        formatted_results.push((result.0, strings[i].as_str()));
    }
    bindings::yield_results(formatted_results.as_slice())
}

pub fn yield_one_request<T, U>(
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
    let request = make_request(
        is_expecting_response,
        target_node,
        target_process,
        &payload,
        context,
    )?;
    yield_results(vec![request]);

    Ok(())
}

pub fn yield_one_response<T, U>(
    json_struct: Option<T>,
    bytes: Option<Vec<u8>>,
    context: Option<U>,  //  ?
) -> anyhow::Result<()>
where
     T: serde::Serialize,
     U: serde::Serialize,
{
    let payload = make_payload(json_struct, bytes)?;
    let request = make_response(&payload, context)?;
    yield_results(vec![request]);

    Ok(())
}
