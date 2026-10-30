//! The claude crate is provider transport: launching Claude, its hooks and
//! messaging sockets, and finding its transcript. The stream-JSON frames are
//! claude-protocol's. Hosting a session and deciding what its traffic means
//! belong to the agent process and the interpreter.

use std::fs;
use std::path::Path;

#[test]
fn the_crate_carries_transport_and_no_session_host() {
    assert!(
        !Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/sdk")
            .exists(),
        "a stream client belongs beside the stream's users, not in the host crate"
    );
    assert!(
        !Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/pty")
            .exists(),
        "amux answers terminal Claude through its hooks; typing into its menus belongs to the recording harness"
    );

    let mut hosts = Vec::new();
    let mut stack = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let text = fs::read_to_string(&path).unwrap();
            for marker in [
                "pub struct Session",
                "pub type EventStream",
                "pub struct Control",
            ] {
                let declared = text.match_indices(marker).any(|(at, _)| {
                    !text[at + marker.len()..]
                        .starts_with(|next: char| next.is_alphanumeric() || next == '_')
                });
                if declared {
                    hosts.push(format!("{}: {marker}", path.display()));
                }
            }
        }
    }
    assert!(
        hosts.is_empty(),
        "session hosting in the claude crate:\n{}",
        hosts.join("\n")
    );
}
