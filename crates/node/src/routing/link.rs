use wire::pb;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectRole {
    Connector,
    Acceptor,
}

pub(crate) fn protocol_error_hello_ack(message: impl Into<String>) -> pb::Message {
    pb::Message {
        body: Some(pb::message::Body::HelloAck(pb::HelloAck {
            outcome: Some(pb::hello_ack::Outcome::Error(protocol_error(message))),
        })),
    }
}

pub(crate) fn protocol_error_link_close(message: impl Into<String>) -> pb::Message {
    pb::Message {
        body: Some(pb::message::Body::LinkClose(pb::LinkClose {
            reason: pb::LinkCloseReason::ProtocolError as i32,
            error: Some(protocol_error(message)),
        })),
    }
}

fn protocol_error(message: impl Into<String>) -> pb::Error {
    pb::Error {
        code: pb::ErrorCode::InvalidArgument as i32,
        message: message.into(),
        details: Vec::new(),
    }
}
