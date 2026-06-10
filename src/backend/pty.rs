//! The PTY runner — spawn under a real TTY, capture output, reap the tree.
//!
//! The PTY is **load-bearing**: agy/codex hang on a piped stdout, so we hand the
//! child a real TTY via `portable-pty`. We capture all output on a reader thread,
//! enforce a wall-clock deadline on the main thread, and on timeout kill the whole
//! **process group** (`SIGTERM` → grace → `SIGKILL`) so ffmpeg grandchildren die too.
//!
//! `run_pty` NEVER returns `Err` for a model/backend failure — the caller classifies
//! the `RawRun`. It returns `Err` only on an OS-level spawn/openpty failure.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

/// agy/codex want a non-trivial window or they wrap output oddly — set via
/// `TIOCSWINSZ` to (rows=50, cols=200).
const PTY_ROWS: u16 = 50;
const PTY_COLS: u16 = 200;

/// Grace between `SIGTERM` and `SIGKILL` when reaping a timed-out tree.
const KILL_GRACE: Duration = Duration::from_secs(3);

/// Poll cadence while waiting on the child against the deadline.
const POLL: Duration = Duration::from_millis(50);

/// The raw outcome of one backend invocation — pre-classification, pre-parse.
#[derive(Debug, Clone)]
pub struct RawRun {
    /// All bytes the child wrote to the PTY (or piped stdout), lossily UTF-8.
    pub stdout: String,
    /// Piped stderr (empty for PTY runs, where it is merged into `stdout`).
    pub stderr: String,
    /// Process exit code, or `None` if we wall-clock-killed it.
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration: Duration,
}

/// Spawn `argv` under a PTY with `env`, capture output, enforce `timeout`.
///
/// `env` fully replaces the child environment (the caller decides what to inherit
/// and what to strip — e.g. claude drops `ANTHROPIC_API_KEY`). Pass `cwd` to set the
/// working directory, or `None` to inherit the parent's.
pub fn run_pty(
    argv: &[String],
    env: &HashMap<String, String>,
    cwd: Option<&Path>,
    timeout: Duration,
) -> std::io::Result<RawRun> {
    assert!(!argv.is_empty(), "run_pty requires a non-empty argv");

    let started = Instant::now();
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: PTY_ROWS,
            cols: PTY_COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(to_io)?;

    let mut cmd = CommandBuilder::new(&argv[0]);
    cmd.args(&argv[1..]);
    // CommandBuilder starts from an empty env; the caller hands us the full set.
    cmd.env_clear();
    for (k, v) in env {
        cmd.env(k, v);
    }
    if let Some(dir) = cwd {
        cmd.cwd(dir);
    }

    let mut child = pair.slave.spawn_command(cmd).map_err(to_io)?;
    // Drop our handle to the slave so the only thing keeping it open is the child;
    // when the child exits, the master read sees EOF (or EIO on macOS).
    drop(pair.slave);

    let pid = child.process_id();
    let mut reader = pair.master.try_clone_reader().map_err(to_io)?;

    // Reader thread: drain the PTY until EOF/EIO, returning the accumulated bytes.
    let reader_handle = thread::spawn(move || {
        let mut acc: Vec<u8> = Vec::with_capacity(64 * 1024);
        let mut buf = [0u8; 65536];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // clean EOF
                Ok(n) => acc.extend_from_slice(&buf[..n]),
                Err(_) => break, // EIO on master = child gone (macOS)
            }
        }
        acc
    });

    // Main thread: wait on the child against the wall-clock deadline.
    let mut timed_out = false;
    let mut exit_code: Option<i32> = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = Some(status.exit_code() as i32);
                break;
            }
            Ok(None) => {}
            Err(_) => break,
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            reap_tree(pid, &mut child);
            break;
        }
        thread::sleep(POLL);
    }

    // Close the master so the reader thread is guaranteed to unblock and finish.
    drop(pair.master);
    let bytes = reader_handle.join().unwrap_or_default();
    let stdout = String::from_utf8_lossy(&bytes).into_owned();

    Ok(RawRun {
        stdout,
        stderr: String::new(),
        exit_code: if timed_out { None } else { exit_code },
        timed_out,
        duration: started.elapsed(),
    })
}

/// Kill the child's whole process group (it + any ffmpeg children), then reap.
/// `SIGTERM` → up to [`KILL_GRACE`] → `SIGKILL`.
fn reap_tree(pid: Option<u32>, child: &mut Box<dyn portable_pty::Child + Send + Sync>) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{killpg, Signal};
        use nix::unistd::Pid;

        if let Some(pid) = pid {
            // portable-pty calls setsid in the child, so child pid == its pgid.
            let pgid = Pid::from_raw(pid as i32);
            let _ = killpg(pgid, Signal::SIGTERM);

            let grace_until = Instant::now() + KILL_GRACE;
            while Instant::now() < grace_until {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    return;
                }
                thread::sleep(POLL);
            }
            let _ = killpg(pgid, Signal::SIGKILL);
        }
    }
    let _ = child.wait();
}

fn to_io<E: std::fmt::Display>(e: E) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inherited_env() -> HashMap<String, String> {
        std::env::vars().collect()
    }

    /// Mechanics: a child that prints and exits cleanly is captured verbatim.
    #[test]
    fn captures_output_and_clean_exit() {
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf 'hello-pty-42'".to_string(),
        ];
        let run = run_pty(&argv, &inherited_env(), None, Duration::from_secs(10)).unwrap();
        assert!(
            run.stdout.contains("hello-pty-42"),
            "stdout: {:?}",
            run.stdout
        );
        assert_eq!(run.exit_code, Some(0));
        assert!(!run.timed_out);
    }

    /// A non-zero exit is reported, not swallowed.
    #[test]
    fn reports_nonzero_exit() {
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "exit 7".to_string(),
        ];
        let run = run_pty(&argv, &inherited_env(), None, Duration::from_secs(10)).unwrap();
        assert_eq!(run.exit_code, Some(7));
        assert!(!run.timed_out);
    }

    /// Wall-clock kill: a long sleep is terminated promptly (process-group kill),
    /// not waited out. This is the load-bearing safety property.
    #[test]
    fn wall_clock_kill_is_prompt() {
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "sleep 30".to_string(),
        ];
        let t0 = Instant::now();
        let run = run_pty(&argv, &inherited_env(), None, Duration::from_millis(800)).unwrap();
        assert!(run.timed_out, "should have timed out");
        assert!(run.exit_code.is_none(), "killed run has no exit code");
        // 800ms deadline + 3s kill grace ⇒ well under 30s if the kill worked.
        assert!(
            t0.elapsed() < Duration::from_secs(8),
            "took {:?}",
            t0.elapsed()
        );
    }

    /// The whole tree dies: a shell that backgrounds a grandchild sleep and waits.
    /// If only the direct child were killed, this would hang to the deadline.
    #[test]
    fn kills_child_tree() {
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "sleep 30 & wait".to_string(),
        ];
        let t0 = Instant::now();
        let run = run_pty(&argv, &inherited_env(), None, Duration::from_millis(800)).unwrap();
        assert!(run.timed_out);
        assert!(
            t0.elapsed() < Duration::from_secs(8),
            "took {:?}",
            t0.elapsed()
        );
    }

    /// Real agy call — proves the PTY is genuinely load-bearing end to end.
    /// Ignored by default (hits a live backend, slow, needs Google login).
    /// Run with: `cargo test --release -- --ignored agy_smoke --nocapture`
    #[test]
    #[ignore]
    fn agy_smoke() {
        let argv = vec![
            "agy".to_string(),
            "-p".to_string(),
            "Reply with exactly the single word: pong".to_string(),
            "--model".to_string(),
            "Gemini 3.5 Flash (High)".to_string(),
            "--dangerously-skip-permissions".to_string(),
            "--print-timeout".to_string(),
            "40s".to_string(),
        ];
        let run = run_pty(&argv, &inherited_env(), None, Duration::from_secs(90)).unwrap();
        eprintln!(
            "--- agy_smoke: exit={:?} timed_out={} dur={:?} ---\n{}\n--- end ---",
            run.exit_code, run.timed_out, run.duration, run.stdout
        );
        assert!(!run.timed_out, "agy timed out");
        assert!(!run.stdout.trim().is_empty(), "agy produced no output");
    }
}
