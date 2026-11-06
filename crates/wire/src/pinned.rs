//! Strings builds of different versions must agree on and no protobuf schema
//! holds, so `just proto-compat` cannot see them. Each line below is one such
//! string where it is defined. Changing one breaks every released build that
//! reads it: an old peer stops pairing, or an old phone stops opening its
//! notifications. A clean-up of these is the next protocol, `amux/2`, served
//! beside this one, as `docs/WIRE.md` says.

use std::path::Path;

/// Each file with the lines it must hold, exactly.
const PINNED: &[(&str, &[&str])] = &[
    (
        "crates/node/src/identity.rs",
        &[r#"pub const QUIC_ALPN: &[u8] = b"amux/1";"#],
    ),
    (
        "crates/node/src/dispatcher.rs",
        &[r#"pub(crate) const NO_PAIRING_WINDOW: &[u8] = b"no pairing window is open";"#],
    ),
    (
        "crates/node/src/discovery/mod.rs",
        &[r#"pub const SERVICE_TYPE: &str = "_amux._udp.local.";"#],
    ),
    (
        "crates/node/src/seal.rs",
        &[r#"const INFO: &[u8] = b"amux push 1";"#],
    ),
    (
        "crates/daemon-core/src/pairing/crypto.rs",
        &[
            r#"const PAIRING_HKDF_SALT: &[u8] = b"amux-pair-spake2-v1";"#,
            r#"pub(crate) const PAIR_CONFIRM_A: &[u8] = b"amux-pair-confirm-A";"#,
            r#"pub(crate) const PAIR_CONFIRM_B: &[u8] = b"amux-pair-confirm-B";"#,
            r#"const PAIR_ID_AAD: &[u8] = b"amux-pair-id";"#,
            r#"const AEAD_A_TO_B_INFO: &[u8] = "aead/A→B".as_bytes();"#,
            r#"const AEAD_B_TO_A_INFO: &[u8] = "aead/B→A".as_bytes();"#,
        ],
    ),
    (
        "crates/wire/src/error.rs",
        &[
            r#"const DOMAIN: &str = "amux.sh";"#,
            r#"const REASON_PREFIX: &str = "ERROR_REASON_";"#,
        ],
    ),
    (
        "crates/wire/src/lib.rs",
        &[
            r#"pub const SURFACE_HEADER: &str = "amux-surface";"#,
            r#"pub const STATUS_TOOL: &str = "status";"#,
            r#"pub const SEND_TOOL: &str = "send";"#,
            r#"pub const ATTACH_TOOL: &str = "attach";"#,
            r#"pub const SPAWN_TOOL: &str = "spawn";"#,
            r#"pub const STOP_TOOL: &str = "stop";"#,
            r#"pub const AGENTS_TOOL: &str = "agents";"#,
            r#"pub const HOSTS_TOOL: &str = "hosts";"#,
        ],
    ),
];

#[test]
fn the_strings_outside_protobuf_have_not_changed() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (file, lines) in PINNED {
        let source = std::fs::read_to_string(root.join(file)).unwrap();
        for line in *lines {
            assert!(
                source.lines().any(|held| held.trim() == *line),
                "{file} no longer says `{line}`; released builds read this string, \
                 see this test's file before changing it"
            );
        }
    }
}
