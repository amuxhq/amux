//! Any line from Claude, on any of its channels: the tolerant decoders
//! return a value or an error, never panic.

#![no_main]

use claude_protocol::{hooks, stream, transcript};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|line: &[u8]| {
    let _ = stream::decode(line);
    let _ = stream::decode_input(line);
    let _ = transcript::decode(line);
    let _ = hooks::decode(line);
});
