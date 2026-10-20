//! Any line from Codex's app server, or from a client of it: the tolerant
//! decoders return a value or an error, never panic.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|line: &[u8]| {
    let _ = codex_protocol::decode(line);
    let _ = codex_protocol::decode_client(line);
});
