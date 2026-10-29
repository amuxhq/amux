//! What tests put at the install path: the two subcommands the harness
//! launches from it, `hooks claude` and `mcp <dir>`, as the amux binary
//! answers them.

use std::io::{Read, Write};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["internal", "hook", "claude"] => {
            let socket = std::env::var_os(claude::hooks::HOOK_SOCKET_ENV)
                .map(PathBuf::from)
                .ok_or("CLAUDE_HOOK_SOCKET is not set")?;
            let mut payload = Vec::new();
            std::io::stdin().read_to_end(&mut payload)?;
            let answer = claude::hooks::forward_from_env(&payload, &socket)?;
            std::io::stdout().write_all(&answer)?;
            Ok(())
        }
        ["internal", "mcp", dir] => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            runtime.block_on(agent::serve_tools(PathBuf::from(dir)))?;
            Ok(())
        }
        other => Err(format!("unknown subcommand {other:?}").into()),
    }
}
