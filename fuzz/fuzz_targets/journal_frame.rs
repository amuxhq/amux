//! Any bytes at the front of a segment: each frame decodes to a whole step
//! inside the bytes, an incomplete start or garbage, never a panic.

#![no_main]

use journal::{Frame, decode_frame};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    let mut at = 0;
    while at < bytes.len() {
        match decode_frame(&bytes[at..]) {
            Frame::Whole(_, length) => {
                assert!(length > 0 && at + length <= bytes.len());
                at += length;
            }
            Frame::Incomplete | Frame::Corrupt => break,
        }
    }
});
