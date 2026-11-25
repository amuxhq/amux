//! Unix's stop events: the three signals. What holds on every OS is in
//! `signal/tests.rs`.

use crate::signal::tests::sees;

/// The roles only Unix has: none.
pub(crate) fn signal_role(rest: &[&str]) {
    panic!("no event {:?}", rest[0]);
}

#[test]
fn terminated_sees_each_signal() {
    assert_eq!(sees("interrupt").0, "Interrupt");
    assert_eq!(sees("terminate").0, "Terminate");
    assert_eq!(sees("hangup").0, "Hangup");
}
