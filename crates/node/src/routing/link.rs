use wire::pb;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectRole {
    Connector,
    Acceptor,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ConnectHandshakeEvent {
    Hello(pb::Hello),
    Accepted(pb::HelloAccepted),
    Rejected(pb::Error),
    PostHandshake(pb::message::Body),
    /// A control message this build does not know, after the handshake:
    /// one a later build added. Adding a control message is allowed only if
    /// a peer that predates it can ignore it, so the link carries on.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ConnectProtocolError {
    #[error("message body is missing")]
    MissingBody,
    #[error("expected hello, got {got}")]
    ExpectedHello { got: &'static str },
    #[error("expected hello_ack, got {got}")]
    ExpectedHelloAck { got: &'static str },
    #[error("hello_ack is missing an accepted/error outcome")]
    MalformedHelloAck,
    #[error("acceptor has not received hello and cannot send hello_ack")]
    AcceptorAckNotReady,
    #[error("acceptor must send hello_ack before receiving {got}")]
    AcceptorMustAckBeforeNextMessage { got: &'static str },
    #[error("illegal post-handshake message: {got}")]
    IllegalPostHandshake { got: &'static str },
    #[error("stream is closed")]
    StreamClosed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectHandshakeState {
    ConnectorAwaitingHelloAck,
    AcceptorAwaitingHello,
    AcceptorReadyToAck,
    Established,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConnectHandshake {
    state: ConnectHandshakeState,
}

impl ConnectHandshake {
    #[cfg(test)]
    pub(crate) fn connector() -> Self {
        Self::new(ConnectRole::Connector)
    }

    #[cfg(test)]
    pub(crate) fn acceptor() -> Self {
        Self::new(ConnectRole::Acceptor)
    }

    pub(crate) fn new(role: ConnectRole) -> Self {
        let state = match role {
            ConnectRole::Connector => ConnectHandshakeState::ConnectorAwaitingHelloAck,
            ConnectRole::Acceptor => ConnectHandshakeState::AcceptorAwaitingHello,
        };
        Self { state }
    }

    pub(crate) fn is_established(&self) -> bool {
        self.state == ConnectHandshakeState::Established
    }

    pub(crate) fn receive(
        &mut self,
        message: pb::Message,
    ) -> Result<ConnectHandshakeEvent, ConnectProtocolError> {
        if self.state == ConnectHandshakeState::Closed {
            return Err(ConnectProtocolError::StreamClosed);
        }

        let Some(body) = message.body else {
            if self.state == ConnectHandshakeState::Established {
                return Ok(ConnectHandshakeEvent::Unknown);
            }
            self.state = ConnectHandshakeState::Closed;
            return Err(ConnectProtocolError::MissingBody);
        };

        match self.state {
            ConnectHandshakeState::ConnectorAwaitingHelloAck => self.connector_receive_first(body),
            ConnectHandshakeState::AcceptorAwaitingHello => self.acceptor_receive_first(body),
            ConnectHandshakeState::AcceptorReadyToAck => {
                self.state = ConnectHandshakeState::Closed;
                Err(ConnectProtocolError::AcceptorMustAckBeforeNextMessage {
                    got: body_name(&body),
                })
            }
            ConnectHandshakeState::Established => match established_body(body) {
                Ok(event) => Ok(event),
                Err(error) => {
                    self.state = ConnectHandshakeState::Closed;
                    Err(error)
                }
            },
            ConnectHandshakeState::Closed => Err(ConnectProtocolError::StreamClosed),
        }
    }

    pub(crate) fn acceptor_ack_sent(&mut self) -> Result<(), ConnectProtocolError> {
        match self.state {
            ConnectHandshakeState::AcceptorReadyToAck => {
                self.state = ConnectHandshakeState::Established;
                Ok(())
            }
            ConnectHandshakeState::Closed => Err(ConnectProtocolError::StreamClosed),
            _ => Err(ConnectProtocolError::AcceptorAckNotReady),
        }
    }

    fn connector_receive_first(
        &mut self,
        body: pb::message::Body,
    ) -> Result<ConnectHandshakeEvent, ConnectProtocolError> {
        let pb::message::Body::HelloAck(ack) = body else {
            self.state = ConnectHandshakeState::Closed;
            return Err(ConnectProtocolError::ExpectedHelloAck {
                got: body_name(&body),
            });
        };

        let Some(outcome) = ack.outcome else {
            self.state = ConnectHandshakeState::Closed;
            return Err(ConnectProtocolError::MalformedHelloAck);
        };

        match outcome {
            pb::hello_ack::Outcome::Accepted(accepted) => {
                self.state = ConnectHandshakeState::Established;
                Ok(ConnectHandshakeEvent::Accepted(accepted))
            }
            pb::hello_ack::Outcome::Error(error) => {
                self.state = ConnectHandshakeState::Closed;
                Ok(ConnectHandshakeEvent::Rejected(error))
            }
        }
    }

    fn acceptor_receive_first(
        &mut self,
        body: pb::message::Body,
    ) -> Result<ConnectHandshakeEvent, ConnectProtocolError> {
        let pb::message::Body::Hello(hello) = body else {
            self.state = ConnectHandshakeState::Closed;
            return Err(ConnectProtocolError::ExpectedHello {
                got: body_name(&body),
            });
        };
        self.state = ConnectHandshakeState::AcceptorReadyToAck;
        Ok(ConnectHandshakeEvent::Hello(hello))
    }
}

pub(crate) fn protocol_error_hello_ack(message: impl Into<String>) -> pb::Message {
    pb::Message {
        body: Some(pb::message::Body::HelloAck(pb::HelloAck {
            outcome: Some(pb::hello_ack::Outcome::Error(protocol_error(message))),
        })),
    }
}

fn established_body(
    body: pb::message::Body,
) -> Result<ConnectHandshakeEvent, ConnectProtocolError> {
    match body {
        pb::message::Body::Hello(_) | pb::message::Body::HelloAck(_) => {
            Err(ConnectProtocolError::IllegalPostHandshake {
                got: body_name(&body),
            })
        }
        body => Ok(ConnectHandshakeEvent::PostHandshake(body)),
    }
}

fn body_name(body: &pb::message::Body) -> &'static str {
    match body {
        pb::message::Body::Hello(_) => "hello",
        pb::message::Body::HelloAck(_) => "hello_ack",
        pb::message::Body::NeighborUp(_) => "neighbor_up",
        pb::message::Body::NeighborDown(_) => "neighbor_down",
        pb::message::Body::Reauth(_) => "reauth",
        pb::message::Body::LinkClose(_) => "link_close",
        pb::message::Body::Addresses(_) => "addresses",
        pb::message::Body::ObservedAddress(_) => "observed_address",
        pb::message::Body::ForwardRefused(_) => "forward_refused",
        pb::message::Body::RefreshCredential(_) => "refresh_credential",
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

#[cfg(test)]
mod tests {
    use super::*;

    fn message(body: pb::message::Body) -> pb::Message {
        pb::Message { body: Some(body) }
    }

    fn hello() -> pb::Message {
        message(pb::message::Body::Hello(pb::Hello {
            supported_protocol_versions: vec![1],
            host: None,
            neighbors: Vec::new(),
            auth_token: None,
            runtime_instance_id: Vec::new(),
            addrs: Vec::new(),
            public_addr: None,
        }))
    }

    fn accepted_ack() -> pb::Message {
        message(pb::message::Body::HelloAck(pb::HelloAck {
            outcome: Some(pb::hello_ack::Outcome::Accepted(pb::HelloAccepted {
                protocol_version: 1,
                host: None,
                neighbors: Vec::new(),
                runtime_instance_id: Vec::new(),
                addrs: Vec::new(),
                observed_addr: None,
                public_addr: None,
            })),
        }))
    }

    fn error_ack() -> pb::Message {
        message(pb::message::Body::HelloAck(pb::HelloAck {
            outcome: Some(pb::hello_ack::Outcome::Error(pb::Error {
                code: pb::ErrorCode::InvalidArgument as i32,
                message: "bad hello".to_string(),
                details: Vec::new(),
            })),
        }))
    }

    fn neighbor_down() -> pb::Message {
        message(pb::message::Body::NeighborDown(pb::NeighborDown {
            host_id: vec![0_u8; 16],
        }))
    }

    #[test]
    fn acceptor_requires_hello_as_first_message() {
        let mut handshake = ConnectHandshake::acceptor();

        assert!(matches!(
            handshake.receive(neighbor_down()),
            Err(ConnectProtocolError::ExpectedHello {
                got: "neighbor_down"
            })
        ));
        assert!(matches!(
            handshake.receive(hello()),
            Err(ConnectProtocolError::StreamClosed)
        ));
    }

    #[test]
    fn acceptor_waits_for_local_ack_before_more_peer_messages() {
        let mut handshake = ConnectHandshake::acceptor();

        assert_eq!(
            handshake.receive(hello()).unwrap(),
            ConnectHandshakeEvent::Hello(pb::Hello {
                supported_protocol_versions: vec![1],
                host: None,
                neighbors: Vec::new(),
                auth_token: None,
                runtime_instance_id: Vec::new(),
                addrs: Vec::new(),
                public_addr: None,
            })
        );
        assert!(!handshake.is_established());
        assert!(matches!(
            handshake.receive(neighbor_down()),
            Err(ConnectProtocolError::AcceptorMustAckBeforeNextMessage {
                got: "neighbor_down"
            })
        ));
        assert!(matches!(
            handshake.acceptor_ack_sent(),
            Err(ConnectProtocolError::StreamClosed)
        ));

        let mut handshake = ConnectHandshake::acceptor();
        handshake.receive(hello()).unwrap();
        handshake.acceptor_ack_sent().unwrap();
        assert!(handshake.is_established());
    }

    #[test]
    fn connector_requires_hello_ack_as_first_message() {
        let mut handshake = ConnectHandshake::connector();

        assert!(matches!(
            handshake.receive(hello()),
            Err(ConnectProtocolError::ExpectedHelloAck { got: "hello" })
        ));
        assert!(matches!(
            handshake.receive(accepted_ack()),
            Err(ConnectProtocolError::StreamClosed)
        ));

        let mut handshake = ConnectHandshake::connector();
        assert!(matches!(
            handshake.receive(accepted_ack()).unwrap(),
            ConnectHandshakeEvent::Accepted(pb::HelloAccepted {
                protocol_version: 1,
                ..
            })
        ));
        assert!(handshake.is_established());
    }

    #[test]
    fn connector_rejection_closes_handshake() {
        let mut handshake = ConnectHandshake::connector();

        assert!(matches!(
            handshake.receive(error_ack()).unwrap(),
            ConnectHandshakeEvent::Rejected(pb::Error { code, .. })
                if code == pb::ErrorCode::InvalidArgument as i32
        ));
        assert!(matches!(
            handshake.receive(neighbor_down()),
            Err(ConnectProtocolError::StreamClosed)
        ));
    }

    #[test]
    fn connector_rejects_malformed_hello_ack() {
        let mut handshake = ConnectHandshake::connector();

        assert!(matches!(
            handshake.receive(message(pb::message::Body::HelloAck(pb::HelloAck {
                outcome: None
            }))),
            Err(ConnectProtocolError::MalformedHelloAck)
        ));
        assert!(matches!(
            handshake.receive(neighbor_down()),
            Err(ConnectProtocolError::StreamClosed)
        ));
    }

    #[test]
    fn established_stream_rejects_late_handshake_messages() {
        let mut handshake = ConnectHandshake::connector();
        handshake.receive(accepted_ack()).unwrap();

        assert!(matches!(
            handshake.receive(hello()),
            Err(ConnectProtocolError::IllegalPostHandshake { got: "hello" })
        ));

        let mut handshake = ConnectHandshake::connector();
        handshake.receive(accepted_ack()).unwrap();
        assert!(matches!(
            handshake.receive(accepted_ack()),
            Err(ConnectProtocolError::IllegalPostHandshake { got: "hello_ack" })
        ));
    }

    #[test]
    fn a_missing_message_body_during_the_handshake_closes_it() {
        let mut handshake = ConnectHandshake::connector();

        assert!(matches!(
            handshake.receive(pb::Message { body: None }),
            Err(ConnectProtocolError::MissingBody)
        ));
        assert!(matches!(
            handshake.receive(accepted_ack()),
            Err(ConnectProtocolError::StreamClosed)
        ));
    }

    /// A message with no body this build knows, once the handshake is
    /// done, is one a later build added: it is ignored and the link
    /// carries on.
    #[test]
    fn an_unknown_message_after_the_handshake_is_ignored() {
        let mut handshake = ConnectHandshake::connector();
        handshake.receive(accepted_ack()).unwrap();

        assert_eq!(
            handshake.receive(pb::Message { body: None }),
            Ok(ConnectHandshakeEvent::Unknown)
        );
        assert!(handshake.is_established());
        assert!(matches!(
            handshake.receive(pb::Message {
                body: Some(pb::message::Body::RefreshCredential(
                    pb::RefreshCredential {}
                )),
            }),
            Ok(ConnectHandshakeEvent::PostHandshake(_))
        ));
    }

    #[test]
    fn acceptor_ack_sent_requires_accepted_hello() {
        let mut handshake = ConnectHandshake::acceptor();

        assert!(matches!(
            handshake.acceptor_ack_sent(),
            Err(ConnectProtocolError::AcceptorAckNotReady)
        ));
    }
}
