//! A child's output, collected once the child exits.
//!
//! `Child::wait_with_output` returns only when both pipes reach EOF. On macOS,
//! Rust std sets close-on-exec on a new pipe in a second call, so a sibling
//! thread's fork in between inherits the write end and holds it open for its
//! whole life. A probe that exited at once then waited on that sibling, and the
//! `srt --version` probe missed its 15 s cap (ADE-259).

use std::io::{self, Read};
use std::process::{Child, Output};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// How long the pipes may stay open after the child exits. Output the child
/// wrote before it exited is read well within this.
const READ_GRACE: Duration = Duration::from_millis(500);

/// `child`'s status and output, once it exits; `None` when it is still
/// running after `timeout`. The caller kills it then.
pub(crate) fn wait_output(mut child: Child, timeout: Duration) -> Option<io::Result<Output>> {
    let stdout = read_chunks(child.stdout.take());
    let stderr = read_chunks(child.stderr.take());
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(child.wait());
    });
    let status = rx.recv_timeout(timeout).ok()?;
    let until = Instant::now() + READ_GRACE;
    Some(status.map(|status| Output {
        status,
        stdout: collect(&stdout, until),
        stderr: collect(&stderr, until),
    }))
}

/// A read error ends the output, as EOF does.
fn read_chunks(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        thread::spawn(move || {
            let mut buf = [0; 8192];
            while let Ok(n @ 1..) = pipe.read(&mut buf) {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
    }
    rx
}

/// Every chunk read until EOF, or until `until` if a pipe is still open then.
fn collect(chunks: &mpsc::Receiver<Vec<u8>>, until: Instant) -> Vec<u8> {
    let mut bytes = Vec::new();
    while let Ok(chunk) = chunks.recv_timeout(until.saturating_duration_since(Instant::now())) {
        bytes.extend(chunk);
    }
    bytes
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::{Duration, Instant};

    fn sh(script: &str, timeout: Duration) -> Result<std::process::Output, String> {
        crate::harness::exec_output(
            "/bin/sh",
            &["-c".to_string(), script.to_string()],
            None,
            timeout,
        )
    }

    /// Defect (ADE-259): the probe waited for its pipes to close, not for its
    /// child to exit. The background `sleep` holds the write ends after `sh`
    /// exits, as a sibling fork that inherited them does.
    #[test]
    fn a_probe_returns_when_its_child_exits_while_another_process_holds_its_pipes() {
        let started = Instant::now();
        let output = sh(
            "echo 0.0.81; echo warn >&2; sleep 6 &",
            Duration::from_secs(3),
        )
        .expect("the probe waited on the process that holds its pipes");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "0.0.81\n");
        assert_eq!(String::from_utf8_lossy(&output.stderr), "warn\n");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    /// Output written to a still-open pipe shortly after the child exits is
    /// kept: the read after exit is bounded, not skipped.
    #[test]
    fn output_written_just_after_the_child_exits_is_kept() {
        let output = sh(
            "echo before; (sleep 0.1; echo after) &",
            Duration::from_secs(3),
        )
        .unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout), "before\nafter\n");
    }

    /// More than a pipe's buffer, so the child exits only after most of it
    /// was read; all of it arrives.
    #[test]
    fn all_of_a_large_output_arrives() {
        let output = sh("head -c 300000 /dev/zero", Duration::from_secs(10)).unwrap();
        assert_eq!(output.stdout.len(), 300_000);
    }

    #[test]
    fn a_child_still_running_at_the_cap_times_out() {
        let err = sh("sleep 6", Duration::from_millis(300)).unwrap_err();
        assert!(err.ends_with("timed out"), "{err}");
    }
}
