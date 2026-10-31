use codex_protocol::RpcError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("process error: {0}")]
    Process(String),

    #[error("transport closed")]
    TransportClosed,

    #[error("JSON-RPC error ({}): {}", .0.code, .0.message)]
    Rpc(RpcError),

    #[error("another consumer holds the thread's events")]
    TurnActive,

    #[error("thread event queue overflowed for thread {0}")]
    ThreadQueueOverflow(String),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}
