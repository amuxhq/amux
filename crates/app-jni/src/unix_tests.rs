//! The wait a watcher makes on the wake descriptor and the stop latch.

use crate::tests::{TempDir, start_config};
use crate::*;

#[test]
fn a_ring_wakes_the_wait_and_a_stop_ends_it() {
    let dir = TempDir::new("app-jni-wake");
    let runtime_handle = start_runtime_from_json(&start_config(&dir)).expect("start runtime");

    // A ring stays readable until a step reads it.
    let _ = with_runtime(runtime_handle, AmuxRuntime::step);
    with_runtime(runtime_handle, AmuxRuntime::ring).expect("runtime");
    assert_eq!(wait_wake(runtime_handle, 5_000), 1);

    // A watcher that never steps keeps waking until the stop, then ends.
    let (ended, end) = std::sync::mpsc::channel();
    let watcher = std::thread::spawn(move || {
        loop {
            let ready = wait_wake(runtime_handle, -1);
            if ready < 0 {
                let _ = ended.send(ready);
                return;
            }
        }
    });
    stop_runtime_handle(runtime_handle);
    assert_eq!(
        end.recv_timeout(std::time::Duration::from_secs(30)),
        Ok(-1),
        "the stop ends the wait"
    );
    watcher.join().expect("watcher");
    assert_eq!(wait_wake(runtime_handle, 0), -1);
}

#[test]
fn an_interrupt_ends_the_wait_even_after_a_step_drained_the_wake() {
    let dir = TempDir::new("app-jni-interrupt");
    let runtime_handle = start_runtime_from_json(&start_config(&dir)).expect("start runtime");
    let interrupt = || {
        get_runtime(runtime_handle)
            .expect("runtime slot")
            .stop
            .set()
    };

    // The stop's signal lands, then a step on the core thread reads the
    // wake descriptor empty before the watcher polls.
    interrupt();
    let _ = with_runtime(runtime_handle, AmuxRuntime::step);
    let (ended, end) = std::sync::mpsc::channel();
    let watcher = std::thread::spawn(move || {
        let _ = ended.send(wait_wake(runtime_handle, -1));
    });
    assert_eq!(
        end.recv_timeout(std::time::Duration::from_secs(5)),
        Ok(-1),
        "the interrupt still ends the wait"
    );
    watcher.join().expect("watcher");
    // The latch stays set: every later wait returns at once, and a
    // second interrupt is harmless.
    interrupt();
    assert_eq!(wait_wake(runtime_handle, -1), -1);
    stop_runtime_handle(runtime_handle);
    assert_eq!(wait_wake(runtime_handle, 0), -1);
}
