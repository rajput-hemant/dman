//! Running one hook command: argv, no shell, no stdin, bounded output.

use std::process::Stdio;
use std::time::Duration;

use dman_config::Hook;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

/// Output kept per stream for the log.
const CAPTURE: usize = 2048;

/// Reads a stream to the end, keeping only the first [`CAPTURE`] bytes (the
/// rest is drained so the child never blocks on a full pipe).
async fn capture<R: AsyncRead + Unpin>(mut r: R) -> String {
    let mut kept = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = CAPTURE.saturating_sub(kept.len());
                kept.extend_from_slice(&buf[..n.min(room)]);
            }
        }
    }
    String::from_utf8_lossy(&kept).trim_end().to_owned()
}

/// `ETXTBSY` ("text file busy") on Linux and macOS.
const ETXTBSY: i32 = 26;

/// Runs `hook` with `env` added to the daemon's environment. Output goes to
/// the debug log. `Err` describes a spawn failure, a non-zero exit or a
/// timeout (the process is killed).
pub async fn run_hook(hook: &Hook, env: &[(&'static str, String)]) -> Result<(), String> {
    let Some((program, args)) = hook.command.split_first() else {
        return Err("empty command".into());
    };
    // A script that was just written can briefly fail with ETXTBSY ("text
    // file busy") while another thread's fork still holds the write handle;
    // retry a few times before giving up.
    let mut attempt = 0;
    let mut child = loop {
        let spawned = Command::new(program)
            .args(args)
            .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn();
        match spawned {
            Ok(child) => break child,
            Err(e) if e.raw_os_error() == Some(ETXTBSY) && attempt < 5 => {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(20 * attempt)).await;
            }
            Err(e) => return Err(format!("cannot start `{program}`: {e}")),
        }
    };
    let out = child.stdout.take().map(|s| tokio::spawn(capture(s)));
    let err = child.stderr.take().map(|s| tokio::spawn(capture(s)));
    let limit = Duration::from_secs(hook.timeout_secs);
    let result = match tokio::time::timeout(limit, child.wait()).await {
        Ok(Ok(status)) => {
            if status.success() {
                Ok(())
            } else {
                Err(format!("`{program}` failed: {status}"))
            }
        }
        Ok(Err(e)) => Err(format!("waiting for `{program}` failed: {e}")),
        Err(_) => {
            let _ = child.kill().await;
            Err(format!(
                "`{program}` timed out after {}s and was killed",
                hook.timeout_secs
            ))
        }
    };
    // A grandchild may keep the pipes open: do not wait for them for long.
    for (label, task) in [("stdout", out), ("stderr", err)] {
        let Some(task) = task else { continue };
        let text = match tokio::time::timeout(Duration::from_secs(1), task).await {
            Ok(Ok(t)) => t,
            _ => String::new(),
        };
        if !text.is_empty() {
            tracing::debug!(hook = %program, "{label}: {text}");
        }
    }
    result
}
