//! What the core's inputs and outputs carry beyond names.
//!
//! Every payload is decoded: the driver turns bytes into these values on the
//! way in and these values into bytes on the way out, so the core never
//! sees a stream. Where the protocol already defines the message, the
//! payload is the generated message type. The rest are the core's own, and
//! a payload no machine in the core reads carries nothing until one does.

/// A link control frame: the hello, its acknowledgement, a neighbor coming
/// up or going down.
pub type LinkFrame = wire::Message;
/// Why a link closed, as the link protocol states it.
pub type LinkCloseReason = wire::LinkCloseReason;
/// One message of a pairing conversation.
pub type PairMessage = wire::PairMessage;
/// An agent process's hello on its control socket.
pub type Hello = wire::AgentHello;
/// A frame to an agent process's control socket.
pub type ControlFrame = wire::CtlFrame;
/// What an agent is started with.
pub type AgentSpec = wire::AgentSpec;
/// The preface that opens a stream over a link, naming where it goes.
pub type StreamPreface = wire::StreamPreface;
/// Why a stream was refused before any byte of it was carried.
pub type StreamRefusal = wire::StreamRefusal;

macro_rules! payload {
    ($($(#[$doc:meta])* $name:ident;)*) => {
        $(
            $(#[$doc])*
            #[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
            pub struct $name;
        )*
    };
}

payload! {
    /// How a link reaches its peer: directly, over SSH or through a relay.
    CarrierKind;
    /// What a channel carries: calls, a session or bulk transfer.
    ChannelClass;
    /// Why a channel closed or could not open.
    ChannelError;
    /// A peer's decoded request on a channel.
    PeerRequest;
    /// A peer's decoded reply, or one item of its streamed reply, to a call
    /// the core made.
    Answer;
    /// A host found on the local network.
    Advertisement;
    /// Who made a client call: a local client or a paired peer's.
    Caller;
    /// A client's decoded request.
    ClientRequest;
    /// One reply, or one item of a streamed reply, to a call.
    Response;
    /// How a call ended.
    Status;
    /// The key a channel's handshake must find at the other end.
    PublicKey;
    /// A blob's content hash.
    BlobHash;
    /// A push notification to send.
    Push;
    /// A batch of analytics events to upload.
    AnalyticsBatch;
    /// What a job produced.
    JobResult;
}
