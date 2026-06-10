//! Piped subprocess runner for backends that behave on pipes (claude, codex).
//! Generalized subprocess handling for backends that behave on pipes.
//!
//! stdin is `/dev/null` (codex reads stdin and would otherwise block); stdout and
//! stderr are captured on reader threads; a wall-clock deadline kills the whole
//! process group (`SIGTERM` → grace → `SIGKILL`), same escalation as the PTY runner.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::pty::RawRun;

const KILL_GRACE: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(50);

/// Spawn `argv` with `env`, stdin=null, capture stdout+stderr, enforce `timeout`.
/// Never errors for a backend failure — only for an OS-level spawn failure.
pub fn run_piped(
    argv: &[String],
    env: &HashMap<String, String>,
    timeout: Duration,
) -> std::io::Result<RawRun> {
    assert!(!argv.is_empty(), "run_piped requires a non-empty argv");
    let started = Instant::now();

    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(unix)]
    unsafe {
        // Own process group so we can kill the whole tree on timeout.
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            // setpgid(0, 0): make this process its own group leader.
            if nix::unistd::setpgid(nix::unistd::Pid::from_raw(0), nix::unistd::Pid::from_raw(0))
                .is_err()
            {
                // Non-fatal: fall back to single-process kill semantics.
            }
            Ok(())
        });
    }

    let mut child = cmd.spawn()?;
    let pid = child.id();

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let out_handle = thread::spawn(move || drain(stdout_pipe.as_mut()));
    let err_handle = thread::spawn(move || drain(stderr_pipe.as_mut()));

    let mut timed_out = false;
    let mut exit_code: Option<i32> = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = status.code();
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

    let stdout = out_handle.join().unwrap_or_default();
    let stderr = err_handle.join().unwrap_or_default();

    Ok(RawRun {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        exit_code: if timed_out { None } else { exit_code },
        timed_out,
        duration: started.elapsed(),
    })
}

fn drain(pipe: Option<&mut impl Read>) -> Vec<u8> {
    let mut acc = Vec::new();
    if let Some(p) = pipe {
        let _ = p.read_to_end(&mut acc);
    }
    acc
}

/// Kill the child's process group, then reap. `SIGTERM` → grace → `SIGKILL`.
fn reap_tree(pid: u32, child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{killpg, Signal};
        use nix::unistd::Pid;

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
    let _ = child.kill();
    let _ = child.wait();
}
