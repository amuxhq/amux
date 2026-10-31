use std::collections::HashMap;
use std::path::PathBuf;

/// How to start Codex's app-server and who to say the client is.
pub struct CodexConfig {
    /// Path to the `codex` binary. Defaults to `"codex"` (found via PATH).
    pub codex_path: Option<PathBuf>,
    /// Model passed explicitly to the Codex CLI before the app-server command.
    pub model: Option<String>,
    /// Working directory for the subprocess.
    pub cwd: Option<PathBuf>,
    /// Client name sent in the initialize handshake.
    pub client_name: String,
    /// Client version sent in the initialize handshake.
    pub client_version: String,
    /// Extra environment variables for the subprocess.
    pub env: Option<HashMap<String, String>>,
    /// Optional JSONL path that receives an exact timestamped tee of JSON-RPC
    /// lines in both directions.
    pub record_io: Option<PathBuf>,
}

// Manual Default because the client identity defaults are not the field
// types' own defaults.
impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            codex_path: None,
            model: None,
            cwd: None,
            client_name: "amux".into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
            env: None,
            record_io: None,
        }
    }
}
