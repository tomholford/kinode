use crate::kernel::component::uq_process::types as wit;
use crate::types as t;

//
// conversions between wit types and kernel types (annoying)
//

pub fn en_wit_address(address: t::Address) -> wit::Address {
    wit::Address {
        node: address.node,
        process: match address.process {
            t::ProcessId::Id(id) => wit::ProcessId::Id(id),
            t::ProcessId::Name(name) => wit::ProcessId::Name(name),
        },
    }
}

pub fn de_wit_address(wit: wit::Address) -> t::Address {
    t::Address {
        node: wit.node,
        process: match wit.process {
            wit::ProcessId::Id(id) => t::ProcessId::Id(id),
            wit::ProcessId::Name(name) => t::ProcessId::Name(name),
        },
    }
}

pub fn en_wit_message(message: t::Message) -> wit::Message {
    match message {
        t::Message::Request(request) => wit::Message::Request(en_wit_request(request)),
        t::Message::Response(response) => match response.0 {
            Ok(r) => wit::Message::Response((Ok(en_wit_response(r)), response.1)),
            Err(error) => wit::Message::Response((Err(en_wit_uqbar_error(error)), response.1)),
        },
    }
}

pub fn de_wit_request(wit: wit::Request) -> t::Request {
    t::Request {
        inherit: wit.inherit,
        expects_response: wit.expects_response,
        ipc: wit.ipc,
        metadata: wit.metadata,
    }
}

pub fn en_wit_request(request: t::Request) -> wit::Request {
    wit::Request {
        inherit: request.inherit,
        expects_response: request.expects_response,
        ipc: request.ipc,
        metadata: request.metadata,
    }
}

pub fn de_wit_response(wit: wit::Response) -> t::Response {
    t::Response {
        ipc: wit.ipc,
        metadata: wit.metadata,
    }
}

pub fn en_wit_response(response: t::Response) -> wit::Response {
    wit::Response {
        ipc: response.ipc,
        metadata: response.metadata,
    }
}

pub fn de_wit_uqbar_error(wit: wit::UqbarError) -> t::UqbarError {
    t::UqbarError {
        kind: wit.kind,
        message: wit.message,
    }
}

pub fn en_wit_uqbar_error(error: t::UqbarError) -> wit::UqbarError {
    wit::UqbarError {
        kind: error.kind,
        message: error.message,
    }
}

pub fn en_wit_network_error(error: t::NetworkError) -> wit::NetworkError {
    wit::NetworkError {
        kind: en_wit_network_error_kind(error.kind),
        message: en_wit_message(error.message),
        payload: en_wit_payload(error.payload),
    }
}

pub fn en_wit_network_error_kind(kind: t::NetworkErrorKind) -> wit::NetworkErrorKind {
    match kind {
        t::NetworkErrorKind::Offline => wit::NetworkErrorKind::Offline,
        t::NetworkErrorKind::Timeout => wit::NetworkErrorKind::Timeout,
    }
}

pub fn de_wit_payload(wit: Option<wit::Payload>) -> Option<t::Payload> {
    match wit {
        None => None,
        Some(wit) => Some(t::Payload {
            mime: wit.mime,
            bytes: wit.bytes,
        }),
    }
}

pub fn en_wit_payload(payload: Option<t::Payload>) -> Option<wit::Payload> {
    match payload {
        None => None,
        Some(payload) => Some(wit::Payload {
            mime: payload.mime,
            bytes: payload.bytes,
        }),
    }
}
