//! Starting Claude Code under a PTY.

use std::ffi::OsString;
use std::path::Path;

use claude::hooks::HOOK_SOCKET_ENV;
use claude::launch::{Launch, pty_spawn_args};

/// Start Claude Code in a PTY of `size`, with the launch's arguments and
/// environment scrub, `env` added to its environment, and its hooks
/// forwarded to `hook_socket`. Environment is the only way to point a
/// terminal session at another API endpoint or key: settings-file `env` does
/// not reach the running session's API client.
pub fn spawn(
    launch: &Launch,
    size: pty_host::PtySize,
    hook_socket: &Path,
    env: &[(OsString, OsString)],
) -> Result<pty_host::PtyProcess, pty_host::PtyError> {
    let mut child_env = env.to_vec();
    child_env.push((
        OsString::from(HOOK_SOCKET_ENV),
        hook_socket.as_os_str().to_owned(),
    ));
    pty_host::spawn(pty_host::PtySpawn {
        command: launch.binary.clone(),
        args: pty_spawn_args(launch),
        cwd: launch.cwd.clone(),
        env: child_env,
        env_remove: launch.env_scrub.iter().map(OsString::from).collect(),
        size,
    })
}
