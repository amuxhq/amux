use super::*;

/// Ringing and draining never block, however often, and the descriptor is
/// one to watch wherever an app's run loop watches one: every OS but
/// Windows, which is told -1. Whether a ring is readable is `unix.rs`'s.
#[test]
fn rings_and_drains_never_block() {
    let wake = Descriptor::new().unwrap();
    assert_eq!(wake.raw() < 0, std::env::consts::OS == "windows");
    for _ in 0..100_000 {
        wake.ring();
    }
    wake.drain();
    wake.drain();
    wake.ring();
    wake.drain();
}
