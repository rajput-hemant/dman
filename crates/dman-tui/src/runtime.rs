//! The async runtime: terminal lifecycle, event sources, the update/render loop
//! and command execution.
//!
//! All inputs (crossterm events, daemon events, connection state, ticks, signals,
//! command results) are funnelled into one mpsc channel of [`Msg`]s. The loop
//! applies each message with [`update`], spawns the returned [`Cmd`]s as tasks
//! (so a slow call never blocks input or rendering) and redraws when the state
//! is dirty, at most every [`MIN_FRAME`].

use std::io::{self, Stdout, stdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event as TermEvent, EventStream, KeyEventKind, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use dman_client::{ConnState, ReconnectingClient};
use dman_proto::method::{Event, Level, ListParams};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

use crate::app::{Cmd, Msg, State, update};
use crate::views;

/// Minimum time between two frames (20 fps).
pub const MIN_FRAME: Duration = Duration::from_millis(50);

const CHANNEL_CAPACITY: usize = 512;

/// Whether the terminal is currently in TUI mode (checked by the panic hook).
static ACTIVE: AtomicBool = AtomicBool::new(false);
static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Restore the terminal if the TUI owns it. Idempotent; safe from a panic hook.
pub fn restore_terminal() {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    let mut out = stdout();
    let _ = execute!(
        out,
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        crossterm::cursor::Show
    );
    let _ = disable_raw_mode();
}

fn install_panic_hook() {
    if HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous(info);
    }));
}

/// Puts the terminal into TUI mode; restores it on drop.
pub struct TerminalGuard {
    _private: (),
}

impl TerminalGuard {
    /// Raw mode, alternate screen, bracketed paste and (optionally) mouse capture.
    ///
    /// # Errors
    /// When the terminal cannot be configured; it is restored first.
    pub fn enter(mouse: bool) -> io::Result<Self> {
        install_panic_hook();
        ACTIVE.store(true, Ordering::SeqCst);
        let guard = Self { _private: () };
        enable_raw_mode()?;
        let mut out = stdout();
        execute!(out, EnterAlternateScreen, EnableBracketedPaste)?;
        if mouse {
            execute!(out, EnableMouseCapture)?;
        }
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// Aborts the wrapped tasks on drop.
struct Tasks(Vec<JoinHandle<()>>);

impl Drop for Tasks {
    fn drop(&mut self) {
        for t in &self.0 {
            t.abort();
        }
    }
}

/// Run the loop until the user quits or a signal arrives.
///
/// # Errors
/// Terminal I/O errors.
pub async fn run_loop(client: ReconnectingClient, mut state: State) -> anyhow::Result<()> {
    // Subscribe before looking at the state so no snapshot is missed.
    let events = client.subscribe();
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let _tasks = Tasks(vec![
        tokio::spawn(input_task(tx.clone())),
        tokio::spawn(events_task(events, tx.clone())),
        tokio::spawn(conn_task(client.clone(), tx.clone())),
        tokio::spawn(tick_task(state.tick, tx.clone())),
        tokio::spawn(signal_task(tx.clone())),
    ]);
    if matches!(*client.state().borrow(), ConnState::Connected { .. }) {
        spawn_cmd(Cmd::Resync, &client, &tx);
    }

    let mouse = state.mouse;
    let _guard = TerminalGuard::enter(mouse)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let size = terminal.size()?;
    state.size = (size.width, size.height);
    state.refresh_list();
    draw(&mut terminal, &mut state)?;
    let mut last_draw = Instant::now();

    loop {
        let msg = if state.dirty {
            tokio::select! {
                m = rx.recv() => m,
                () = tokio::time::sleep_until(last_draw + MIN_FRAME) => {
                    draw(&mut terminal, &mut state)?;
                    last_draw = Instant::now();
                    continue;
                }
            }
        } else {
            rx.recv().await
        };
        let Some(msg) = msg else { break };
        let was_dirty = state.dirty;
        let cmds = update(&mut state, msg);
        state.dirty |= was_dirty;
        let mut quit = false;
        for cmd in cmds {
            if cmd == Cmd::Quit {
                quit = true;
            } else {
                spawn_cmd(cmd, &client, &tx);
            }
        }
        if quit {
            break;
        }
    }
    Ok(())
}

fn draw(terminal: &mut Terminal<CrosstermBackend<Stdout>>, state: &mut State) -> io::Result<()> {
    terminal.draw(|f| views::render(state, f))?;
    state.dirty = false;
    Ok(())
}

fn spawn_cmd(cmd: Cmd, client: &ReconnectingClient, tx: &mpsc::Sender<Msg>) {
    let client = client.clone();
    let tx = tx.clone();
    tokio::spawn(async move {
        let msg = match cmd {
            Cmd::Api(call) => {
                let result = call.clone().execute(&client).await;
                Msg::ActionResult { call, result }
            }
            Cmd::Resync => resync(&client).await,
            Cmd::ReadDir(path) => Msg::LocalDir(
                blocking(move || views::add::browser::read_local_dir(path.as_deref()))
                    .await
                    .and_then(|r| r),
            ),
            Cmd::CheckPaths(paths) => Msg::PathsChecked(
                blocking(move || {
                    paths
                        .into_iter()
                        .map(|p| {
                            let s = views::add::source::check_path(&p);
                            (p, s)
                        })
                        .collect()
                })
                .await
                .unwrap_or_default(),
            ),
            Cmd::ReadFiles(paths) => Msg::FilesRead(
                blocking(move || {
                    paths
                        .into_iter()
                        .map(|p| {
                            let r = views::add::source::read_file(&p);
                            (p, r)
                        })
                        .collect()
                })
                .await
                .unwrap_or_default(),
            ),
            Cmd::ReadClipboard => Msg::Clipboard(blocking(read_clipboard).await.and_then(|r| r)),
            Cmd::ReadProviderFile(path) => Msg::ProviderFile(
                blocking(move || read_provider_file(&path))
                    .await
                    .and_then(|r| r),
            ),
            Cmd::WriteProviderFile { path, content } => Msg::ProviderFileWritten(
                blocking(move || write_provider_file(&path, &content))
                    .await
                    .and_then(|r| r),
            ),
            Cmd::Notify { title, body } => {
                // Best effort: a headless machine has no notification service.
                let _ = blocking(move || {
                    if let Err(e) = notify_rust::Notification::new()
                        .appname("dman")
                        .summary(&title)
                        .body(&body)
                        .show()
                    {
                        tracing::debug!("desktop notification not shown: {e}");
                    }
                })
                .await;
                return;
            }
            Cmd::Quit => return,
        };
        let _ = tx.send(msg).await;
    });
}

/// Run blocking work off the async threads.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("background task failed: {e}"))
}

/// `~/x` to the home directory.
fn expand_home(path: &str) -> std::path::PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => std::path::Path::new(&home).join(rest),
        _ => std::path::PathBuf::from(path),
    }
}

/// Largest providers file accepted for import.
const MAX_PROVIDER_FILE: u64 = 1 << 20;

fn read_provider_file(path: &str) -> Result<(String, String), String> {
    let full = expand_home(path);
    let meta = std::fs::metadata(&full).map_err(|e| format!("{path}: {e}"))?;
    if meta.len() > MAX_PROVIDER_FILE {
        return Err(format!("{path} is too big for a providers file"));
    }
    std::fs::read_to_string(&full)
        .map(|t| (path.to_owned(), t))
        .map_err(|e| format!("{path}: {e}"))
}

/// Writes a new file only: an existing file is never overwritten.
fn write_provider_file(path: &str, content: &str) -> Result<String, String> {
    use std::io::Write;
    let full = expand_home(path);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&full)
        .map_err(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                format!("{path} already exists; choose another name")
            } else {
                format!("{path}: {e}")
            }
        })?;
    f.write_all(content.as_bytes())
        .map_err(|e| format!("{path}: {e}"))?;
    Ok(path.to_owned())
}

/// Text from the system clipboard (decision D17). Fails on headless hosts and
/// over SSH, where the terminal's own paste has to be used.
fn read_clipboard() -> Result<String, String> {
    arboard::Clipboard::new()
        .and_then(|mut c| c.get_text())
        .map_err(|e| e.to_string())
}

async fn resync(client: &ReconnectingClient) -> Msg {
    let fetched = async {
        let jobs = client.list(ListParams::default()).await?;
        let stats = client.stats().await?;
        let limits = client.limits_get().await?;
        Ok::<_, dman_client::ClientError>(Event::Snapshot {
            jobs,
            stats,
            limits,
        })
    }
    .await;
    match fetched {
        Ok(e) => Msg::Event(e),
        Err(e) => Msg::Event(Event::Message {
            level: Level::Error,
            text: format!("Reload failed: {e}"),
        }),
    }
}

async fn input_task(tx: mpsc::Sender<Msg>) {
    let mut stream = EventStream::new();
    while let Some(ev) = stream.next().await {
        let msg = match ev {
            Ok(TermEvent::Key(k)) if k.kind != KeyEventKind::Release => Msg::Key(k),
            Ok(TermEvent::Mouse(m))
                if !matches!(m.kind, MouseEventKind::Moved | MouseEventKind::Drag(_)) =>
            {
                Msg::Mouse(m)
            }
            Ok(TermEvent::Paste(s)) => Msg::Paste(s),
            Ok(TermEvent::Resize(w, h)) => Msg::Resize(w, h),
            Ok(_) => continue,
            Err(e) => {
                tracing::warn!(error = %e, "terminal input failed");
                Msg::Terminate
            }
        };
        let stop = msg == Msg::Terminate;
        if tx.send(msg).await.is_err() || stop {
            break;
        }
    }
}

async fn events_task(mut events: broadcast::Receiver<Event>, tx: mpsc::Sender<Msg>) {
    loop {
        let msg = match events.recv().await {
            Ok(e) => Msg::Event(e),
            Err(broadcast::error::RecvError::Lagged(_)) => Msg::Lagged,
            Err(broadcast::error::RecvError::Closed) => break,
        };
        if tx.send(msg).await.is_err() {
            break;
        }
    }
}

async fn conn_task(client: ReconnectingClient, tx: mpsc::Sender<Msg>) {
    let mut watch = client.state();
    loop {
        let s = watch.borrow_and_update().clone();
        if tx.send(Msg::Conn(s)).await.is_err() || watch.changed().await.is_err() {
            break;
        }
    }
}

async fn tick_task(every: Duration, tx: mpsc::Sender<Msg>) {
    let mut interval = tokio::time::interval(every);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        if tx.send(Msg::Tick).await.is_err() {
            break;
        }
    }
}

#[cfg(unix)]
async fn signal_task(tx: mpsc::Sender<Msg>) {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut term), Ok(mut hup)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::hangup()),
    ) else {
        tracing::warn!("cannot install signal handlers");
        return;
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = hup.recv() => {}
    }
    let _ = tx.send(Msg::Terminate).await;
}

#[cfg(not(unix))]
async fn signal_task(tx: mpsc::Sender<Msg>) {
    if tokio::signal::ctrl_c().await.is_ok() {
        let _ = tx.send(Msg::Terminate).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_files_round_trip_and_never_overwrite() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("providers.toml");
        let path = path.to_string_lossy().into_owned();
        assert_eq!(
            write_provider_file(&path, "[[provider]]\n"),
            Ok(path.clone())
        );
        let err = write_provider_file(&path, "other").expect_err("exists");
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(
            read_provider_file(&path),
            Ok((path.clone(), "[[provider]]\n".to_owned()))
        );
        let missing = dir.path().join("missing.toml");
        assert!(read_provider_file(&missing.to_string_lossy()).is_err());
    }
}
