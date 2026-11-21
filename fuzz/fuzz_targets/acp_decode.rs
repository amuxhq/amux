//! Any line from an ACP agent, or from a client of one: the tolerant
//! decoders return a value or an error, never panic, and what they return
//! encodes.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|line: &[u8]| {
    if let Ok(message) = acp_protocol::decode(line) {
        let _ = acp_protocol::encode_server(&message);
    }
    if let Ok(message) = acp_protocol::decode_client(line) {
        let _ = acp_protocol::encode(&message);
    }
    let _ = acp_protocol::strict(line);
    let _ = acp_protocol::fields(line);
});
