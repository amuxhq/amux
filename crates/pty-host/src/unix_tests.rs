//! A hosted process on Unix, run through `sh`, and its process group.

use crate::*;

fn shell(script: &str) -> PtySpawn {
    PtySpawn {
        command: PathBuf::from("sh"),
        args: vec!["-c".to_string(), script.to_string()],
        cwd: std::env::temp_dir(),
        env: Vec::new(),
        env_remove: Vec::new(),
        size: PtySize::default(),
    }
}

async fn read_until(output: &mut mpsc::Receiver<Bytes>, needle: &[u8]) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut seen = Vec::new();
        while !seen.windows(needle.len()).any(|window| window == needle) {
            let bytes = output.recv().await.expect("PTY output ended early");
            seen.extend_from_slice(&bytes);
        }
        seen
    })
    .await
    .expect("timed out waiting for PTY output")
}

#[tokio::test]
async fn spawns_and_exposes_output_and_pid() {
    let mut process = spawn(shell("printf spawned")).unwrap();
    assert!(process.handle.pid() > 0);
    let mut output = process.handle.output();
    let seen = read_until(&mut output, b"spawned").await;
    assert!(seen.windows(7).any(|window| window == b"spawned"));
    assert!(process.exit.wait().await.success());
}

#[tokio::test]
async fn writes_bytes_to_an_echoing_child() {
    let process = spawn(shell("stty -echo; printf ready; cat")).unwrap();
    let mut output = process.handle.output();
    read_until(&mut output, b"ready").await;
    process.handle.write(b"echo me\n").await.unwrap();
    let seen = read_until(&mut output, b"echo me").await;
    assert!(seen.windows(7).any(|window| window == b"echo me"));
    terminate(&process, Terminate::Kill).await;
    assert!(process.exit.status().is_some());
}

#[tokio::test]
async fn signals_the_process_group_without_a_runtime() {
    let mut process = spawn(shell("sleep 30")).unwrap();
    let handle = process.handle.clone();
    std::thread::spawn(move || handle.signal_process_group(ProcessGroupSignal::Terminate))
        .join()
        .unwrap()
        .unwrap();

    let status = tokio::time::timeout(Duration::from_secs(3), process.exit.wait())
        .await
        .expect("process group did not terminate after synchronous signal");
    assert!(!status.success());
}

#[tokio::test]
async fn applies_environment_additions_and_removals() {
    let mut spec = shell("printf '%s/%s' \"${PTY_HOST_ADDED-unset}\" \"${HOME-unset}\"");
    spec.env.push(("PTY_HOST_ADDED".into(), "added".into()));
    spec.env_remove.push("HOME".into());
    let mut process = spawn(spec).unwrap();
    let mut output = process.handle.output();
    let seen = read_until(&mut output, b"added/unset").await;
    assert!(seen.windows(11).any(|window| window == b"added/unset"));
    assert!(process.exit.wait().await.success());
}

#[tokio::test]
async fn resizes_the_terminal() {
    let mut process = spawn(shell("read line; stty size")).unwrap();
    let mut output = process.handle.output();
    process
        .handle
        .resize(PtySize { rows: 41, cols: 97 })
        .unwrap();
    process.handle.write(b"measure\n").await.unwrap();
    let seen = read_until(&mut output, b"41 97").await;
    assert!(seen.windows(5).any(|window| window == b"41 97"));
    assert!(process.exit.wait().await.success());
}

#[tokio::test]
async fn reports_exit_status_once_and_after_wait() {
    let mut process = spawn(shell("exit 7")).unwrap();
    let status = process.exit.wait().await;
    assert_eq!(status.exit_code(), 7);
    assert_eq!(process.exit.status().unwrap().exit_code(), 7);
    assert_eq!(process.exit.wait().await.exit_code(), 7);
}

/// A killed process whose parent died first is adopted and stays a
/// zombie until its new parent reaps it. On Linux that parent can be a
/// subreaper that reaps late (a build worker running the test), and
/// signal 0 still finds a zombie; it is dead all the same.
fn is_zombie(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(')')
                .map(|(_, rest)| rest.trim_start().starts_with('Z'))
        })
        .unwrap_or(false)
}

#[tokio::test]
async fn termination_kills_the_whole_process_group() {
    let process = spawn(shell("trap '' TERM; sleep 30 & echo $!; wait")).unwrap();
    let mut output = process.handle.output();
    let seen = read_until(&mut output, b"\r\n").await;
    let text = String::from_utf8_lossy(&seen);
    let child_pid: i32 = text
        .split(|character: char| !character.is_ascii_digit())
        .find(|part| !part.is_empty())
        .unwrap()
        .parse()
        .unwrap();

    let status = tokio::time::timeout(
        Duration::from_secs(3),
        terminate(
            &process,
            Terminate::Graceful {
                grace: Duration::from_millis(50),
            },
        ),
    )
    .await
    .expect("process group did not terminate");
    assert!(!status.success());

    let child_gone = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let result = unsafe { libc::kill(child_pid, 0) };
            if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                break;
            }
            if is_zombie(child_pid) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(child_gone.is_ok(), "descendant survived group termination");
}
