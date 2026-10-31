#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectRole {
    Connector,
    Acceptor,
}

pub(crate) fn protocol_error_hello_ack(message: impl Into<String>) -> wire::Message {
    wire::Message {
        body: Some(wire::message::Body::HelloAck(wire::HelloAck {
            outcome: Some(wire::hello_ack::Outcome::Error(protocol_error(message))),
        })),
    }
}

pub(crate) fn protocol_error_link_close(message: impl Into<String>) -> wire::Message {
    wire::Message {
        body: Some(wire::message::Body::LinkClose(wire::LinkClose {
            reason: wire::LinkCloseReason::ProtocolError as i32,
            error: Some(protocol_error(message)),
        })),
    }
}

fn protocol_error(message: impl Into<String>) -> wire::Error {
    wire::Error {
        code: wire::ErrorCode::InvalidArgument as i32,
        message: message.into(),
        details: Vec::new(),
    }
}
