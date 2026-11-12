use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;

use crate::args::{DownloadSpec, build_args, final_file_path};
use crate::error::{Error, Result};
use crate::probe::error_message;
use crate::progress::{Progress, parse_item_line, parse_progress_line};
use crate::tools::Tools;

const TAIL_LINES: usize = 30;
const KILL_GRACE: Duration = Duration::from_secs(3);
const READER_JOIN: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    /// Final file(s) exist (as printed by `after_move:filepath`).
    Finished { paths: Vec<PathBuf> },
    Failed {
        exit_code: Option<i32>,
        message: String,
    },
    /// Exit code 0 but the streams were left as separate `name.fN.ext` files (no ffmpeg to
    /// merge). `finished` holds any other items that did complete (playlists).
    Split {
        files: Vec<PathBuf>,
        finished: Vec<PathBuf>,
    },
    /// [`Running::stop`] was requested; partial files stay for `--continue`.
    Stopped,
}

/// A running yt-dlp process. Dropping it kills the process.
pub struct Running {
    /// Progress events (lossy: dropped if the consumer falls behind). Closes when the process ends.
    pub progress: mpsc::Receiver<Progress>,
    stop: Arc<Notify>,
    task: Option<JoinHandle<Outcome>>,
    /// Process group led by the yt-dlp child (it also holds ffmpeg and friends).
    pgid: Option<u32>,
}

/// Cloneable handle to stop a [`Running`] from elsewhere.
#[derive(Clone)]
pub struct StopHandle(Arc<Notify>);

impl StopHandle {
    /// SIGTERM, then SIGKILL after 3 s. [`Running::wait`] then yields [`Outcome::Stopped`].
    pub fn stop(&self) {
        self.0.notify_one();
    }
}

impl Running {
    pub async fn next_progress(&mut self) -> Option<Progress> {
        self.progress.recv().await
    }

    pub fn stop(&self) {
        self.stop.notify_one();
    }

    pub fn stop_handle(&self) -> StopHandle {
        StopHandle(self.stop.clone())
    }

    /// Wait for the process to end. Drain [`Running::progress`] concurrently if you want events.
    pub async fn wait(mut self) -> Outcome {
        // The handle stays in `self` while waiting so that dropping this future
        // still aborts the task and kills the process group (see `Drop`).
        let task = self.task.as_mut().expect("wait called once");
        let result = task.await;
        self.task = None;
        result.unwrap_or_else(|e| Outcome::Failed {
            exit_code: None,
            message: format!("supervisor task failed: {e}"),
        })
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(t) = &self.task {
            // Only while the supervisor still runs: afterwards the group is gone
            // and its id may already belong to someone else.
            let live = !t.is_finished();
            // Aborting drops the Child, which has kill_on_drop set (the child only).
            t.abort();
            if live && let Some(pgid) = self.pgid {
                // Take the grandchildren (ffmpeg, ...) down too.
                signal_group(pgid, Stop::Force);
            }
        }
    }
}

/// Start a download. Pause = `stop()`; resume = `spawn` again with the same spec.
pub fn spawn(spec: &DownloadSpec, tools: &Tools, job_tag: &str) -> Result<Running> {
    spec.validate()?;
    if job_tag.is_empty()
        || !job_tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
    {
        return Err(Error::InvalidSpec("job tag must be [A-Za-z0-9_-]+".into()));
    }
    let ytdlp = tools.ytdlp.as_ref().ok_or(Error::YtdlpMissing)?;
    std::fs::create_dir_all(&spec.dir).map_err(|source| Error::Spawn {
        tool: "yt-dlp",
        source,
    })?;
    // --print-to-file appends; start from a clean slate (our own marker file only).
    let final_file = final_file_path(spec, job_tag);
    let _ = std::fs::remove_file(&final_file);

    let mut cmd = Command::new(&ytdlp.path);
    cmd.args(build_args(spec, tools, job_tag))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Own process group, so stopping the job also stops yt-dlp's children
    // (ffmpeg). On Windows the tree is found through the process id instead.
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    {
        // No console window for the child.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|source| Error::Spawn {
        tool: "yt-dlp",
        source,
    })?;
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let pid = child.id();

    let tail: Arc<Mutex<VecDeque<String>>> = Arc::default();
    let (tx, rx) = mpsc::channel(256);

    let out_task = {
        let tail = tail.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            let mut item = None;
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(mut p) = parse_progress_line(&line) {
                    p.item = item;
                    let _ = tx.try_send(p);
                } else if line.trim_start().starts_with('{') {
                    continue;
                } else {
                    if let Some(i) = parse_item_line(&line) {
                        item = Some(i);
                    }
                    push_tail(&tail, line);
                }
            }
        })
    };
    let err_task = {
        let tail = tail.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                push_tail(&tail, line);
            }
        })
    };

    let stop = Arc::new(Notify::new());
    let stop2 = stop.clone();
    let dir = spec.dir.clone();
    let task = tokio::spawn(async move {
        let mut stopped = false;
        let status = tokio::select! {
            s = child.wait() => s,
            _ = stop2.notified() => {
                stopped = true;
                terminate(&mut child, pid).await
            }
        };
        for t in [out_task, err_task] {
            // A grandchild (ffmpeg) may keep the pipes open; don't wait forever.
            let abort = t.abort_handle();
            if tokio::time::timeout(READER_JOIN, t).await.is_err() {
                abort.abort();
            }
        }
        if stopped {
            return Outcome::Stopped;
        }
        let lines: Vec<String> = tail
            .lock()
            .map(|t| t.iter().cloned().collect())
            .unwrap_or_default();
        match status {
            Ok(st) => {
                let marker = std::fs::read_to_string(&final_file).unwrap_or_default();
                let outcome = classify_outcome(&dir, &marker, st.code(), &lines);
                let _ = std::fs::remove_file(&final_file);
                outcome
            }
            Err(e) => Outcome::Failed {
                exit_code: None,
                message: format!("waiting for yt-dlp failed: {e}"),
            },
        }
    });

    Ok(Running {
        progress: rx,
        stop,
        task: Some(task),
        pgid: pid,
    })
}

fn push_tail(tail: &Mutex<VecDeque<String>>, line: String) {
    if line.trim().is_empty() {
        return;
    }
    if let Ok(mut t) = tail.lock() {
        if t.len() == TAIL_LINES {
            t.pop_front();
        }
        t.push_back(line);
    }
}

/// Ask the process tree to stop, force it after the grace period.
async fn terminate(
    child: &mut Child,
    pid: Option<u32>,
) -> std::io::Result<std::process::ExitStatus> {
    if let Some(pid) = pid {
        signal_group(pid, Stop::Graceful);
        if let Ok(st) = tokio::time::timeout(KILL_GRACE, child.wait()).await {
            return st;
        }
        signal_group(pid, Stop::Force);
    }
    let _ = child.start_kill();
    child.wait().await
}

/// How to stop a process tree.
#[derive(Debug, Clone, Copy)]
enum Stop {
    /// SIGTERM on Unix; a plain `taskkill /T` on Windows.
    Graceful,
    /// SIGKILL on Unix; `taskkill /T /F` on Windows.
    Force,
}

/// Stop the process group led by `pid`. A group that is already gone is expected
/// and ignored; any other failure is only logged (best effort).
#[cfg(unix)]
fn signal_group(pid: u32, stop: Stop) {
    use nix::errno::Errno;
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;
    let Ok(raw) = i32::try_from(pid) else {
        return;
    };
    let sig = match stop {
        Stop::Graceful => Signal::SIGTERM,
        Stop::Force => Signal::SIGKILL,
    };
    match killpg(Pid::from_raw(raw), sig) {
        Ok(()) | Err(Errno::ESRCH) => {}
        Err(err) => tracing::debug!(pgid = raw, ?sig, %err, "killpg failed"),
    }
}

/// Stop the process `pid` and its children with `taskkill /T`. Windows has no
/// process groups that can be signalled, so the tree is walked by process id.
#[cfg(windows)]
fn signal_group(pid: u32, stop: Stop) {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut cmd = std::process::Command::new("taskkill");
    cmd.args(["/PID", &pid.to_string(), "/T"]);
    if matches!(stop, Stop::Force) {
        cmd.arg("/F");
    }
    let result = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    if let Err(err) = result {
        tracing::debug!(pid, ?stop, %err, "taskkill failed");
    }
}

/// Decide the result of a finished process. `marker` is the content of the `.dman-<tag>.final`
/// file (one path per line).
pub fn classify_outcome(
    dir: &Path,
    marker: &str,
    exit_code: Option<i32>,
    tail: &[String],
) -> Outcome {
    if exit_code != Some(0) {
        let from_tail = error_message(&tail.join("\n"));
        let message = if tail.is_empty() {
            match exit_code {
                Some(c) => format!("yt-dlp exited with status {c}"),
                None => "yt-dlp was terminated by a signal".to_string(),
            }
        } else {
            from_tail
        };
        return Outcome::Failed { exit_code, message };
    }
    let mut existing = Vec::new();
    let mut split = Vec::new();
    for line in marker.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let p = PathBuf::from(line);
        let p = if p.is_absolute() { p } else { dir.join(p) };
        if p.is_file() {
            existing.push(p);
        } else {
            split.extend(find_split_files(&p));
        }
    }
    split.sort();
    split.dedup();
    if !split.is_empty() {
        return Outcome::Split {
            files: split,
            finished: existing,
        };
    }
    if existing.is_empty() {
        return Outcome::Failed {
            exit_code,
            message: "yt-dlp exited successfully but produced no output file".into(),
        };
    }
    Outcome::Finished { paths: existing }
}

/// For a missing `dir/name.ext`, find `dir/name.fN.*` leftovers (unmerged streams).
fn find_split_files(missing: &Path) -> Vec<PathBuf> {
    let (Some(parent), Some(stem)) = (missing.parent(), missing.file_stem()) else {
        return vec![];
    };
    let prefix = format!("{}.f", stem.to_string_lossy());
    let Ok(rd) = std::fs::read_dir(parent) else {
        return vec![];
    };
    rd.flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(rest) = name.strip_prefix(&prefix) else {
                return false;
            };
            let digits = rest.chars().take_while(char::is_ascii_digit).count();
            digits > 0
                && rest[digits..].starts_with('.')
                && !name.ends_with(".part")
                && !name.ends_with(".ytdl")
        })
        .map(|e| e.path())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(p: &Path) {
        fs::write(p, b"x").unwrap();
    }

    #[test]
    fn finished_when_file_exists() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a [1].mp4");
        touch(&f);
        let m = format!("{}\n", f.display());
        assert_eq!(
            classify_outcome(d.path(), &m, Some(0), &[]),
            Outcome::Finished { paths: vec![f] }
        );
    }

    #[test]
    fn split_files_detected() {
        let d = tempfile::tempdir().unwrap();
        let v = d.path().join("a [1].f137.mp4");
        let a = d.path().join("a [1].f140.m4a");
        touch(&v);
        touch(&a);
        touch(&d.path().join("a [1].f299.mp4.part"));
        touch(&d.path().join("other.f1.mp4"));
        let m = format!("{}\n", d.path().join("a [1].mp4").display());
        assert_eq!(
            classify_outcome(d.path(), &m, Some(0), &[]),
            Outcome::Split {
                files: vec![v, a],
                finished: vec![]
            }
        );
    }

    #[test]
    fn split_with_other_finished_item() {
        let d = tempfile::tempdir().unwrap();
        let ok = d.path().join("ok.mp4");
        let s0 = d.path().join("bad.f0.mp4");
        touch(&ok);
        touch(&s0);
        let m = format!("{}\n{}\n", ok.display(), d.path().join("bad.mp4").display());
        assert_eq!(
            classify_outcome(d.path(), &m, Some(0), &[]),
            Outcome::Split {
                files: vec![s0],
                finished: vec![ok]
            }
        );
    }

    #[test]
    fn failure_uses_error_line() {
        let d = tempfile::tempdir().unwrap();
        let tail = vec![
            "[generic] x".to_string(),
            "ERROR: unable to download: HTTP Error 404".to_string(),
            "trailing".to_string(),
        ];
        assert_eq!(
            classify_outcome(d.path(), "", Some(1), &tail),
            Outcome::Failed {
                exit_code: Some(1),
                message: "ERROR: unable to download: HTTP Error 404".into()
            }
        );
    }

    #[test]
    fn success_without_output_is_failure() {
        let d = tempfile::tempdir().unwrap();
        assert!(matches!(
            classify_outcome(d.path(), "", Some(0), &[]),
            Outcome::Failed { .. }
        ));
    }

    #[tokio::test]
    async fn spawn_requires_ytdlp_and_valid_tag() {
        let d = tempfile::tempdir().unwrap();
        let spec = DownloadSpec::new(url::Url::parse("https://e.com/v").unwrap(), d.path());
        assert!(matches!(
            spawn(&spec, &Tools::default(), "j"),
            Err(Error::YtdlpMissing)
        ));
        let t = Tools {
            ytdlp: Some(crate::ToolInfo {
                path: "/bin/true".into(),
                version: "x".into(),
            }),
            ffmpeg: None,
        };
        assert!(matches!(
            spawn(&spec, &t, "../x"),
            Err(Error::InvalidSpec(_))
        ));
    }

    /// A pid that exists and is not a zombie.
    #[cfg(target_os = "linux")]
    fn alive(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|s| s.rsplit_once(')').map(|(_, r)| r.trim_start().to_owned()))
            .is_some_and(|r| !r.starts_with('Z'))
    }

    /// Dropping a `Running` must kill grandchildren too (yt-dlp's ffmpeg).
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn drop_kills_the_whole_process_group() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let pidfile = d.path().join("grandchild.pid");
        let script = d.path().join("fake-yt-dlp");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nsleep 300 &\necho $! > '{}'\nwait\n",
                pidfile.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let tools = Tools {
            ytdlp: Some(crate::tools::ToolInfo {
                path: script,
                version: "0".into(),
            }),
            ffmpeg: None,
        };
        let spec = DownloadSpec::new(
            url::Url::parse("https://example.com/v").unwrap(),
            d.path().join("dl"),
        );
        let run = spawn(&spec, &tools, "t").unwrap();
        let mut pid = None;
        for _ in 0..100 {
            if let Some(p) = fs::read_to_string(&pidfile)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
            {
                pid = Some(p);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let pid = pid.expect("fake yt-dlp started its child");
        assert!(alive(pid));
        drop(run);
        for _ in 0..100 {
            if !alive(pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("grandchild {pid} survived dropping Running");
    }
}
