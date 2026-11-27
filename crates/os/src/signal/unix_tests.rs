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

/// A resize of the terminal (SIGWINCH) is heard.
#[test]
fn a_resize_is_heard() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut resizes = crate::signal::Resizes::listen().unwrap();
        // SAFETY: raise only sends a signal to this process, whose
        // SIGWINCH tokio now handles (its default is to ignore it).
        assert_eq!(unsafe { libc::raise(libc::SIGWINCH) }, 0);
        let heard = tokio::time::timeout(std::time::Duration::from_secs(5), resizes.recv()).await;
        assert_eq!(heard, Ok(Some(())));
    });
}
