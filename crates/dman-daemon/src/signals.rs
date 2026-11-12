//! The OS requests that stop the daemon: SIGINT and SIGTERM on Unix (SIGHUP is
//! ignored so closing the terminal that started it does not stop it); Ctrl-C,
//! Ctrl-Break, closing the console and logoff/shutdown on Windows.

use anyhow::Context as _;

/// Stop requests from the operating system.
pub struct StopSignals {
    #[cfg(unix)]
    int: tokio::signal::unix::Signal,
    #[cfg(unix)]
    term: tokio::signal::unix::Signal,
    #[cfg(unix)]
    hup: tokio::signal::unix::Signal,
    #[cfg(windows)]
    c: tokio::signal::windows::CtrlC,
    #[cfg(windows)]
    brk: tokio::signal::windows::CtrlBreak,
    #[cfg(windows)]
    close: tokio::signal::windows::CtrlClose,
    #[cfg(windows)]
    shutdown: tokio::signal::windows::CtrlShutdown,
}

impl StopSignals {
    /// Installs the handlers.
    pub fn install() -> anyhow::Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let ctx = "cannot install signal handlers";
            Ok(Self {
                int: signal(SignalKind::interrupt()).context(ctx)?,
                term: signal(SignalKind::terminate()).context(ctx)?,
                hup: signal(SignalKind::hangup()).context(ctx)?,
            })
        }
        #[cfg(windows)]
        {
            use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_shutdown};
            let ctx = "cannot install console control handlers";
            Ok(Self {
                c: ctrl_c().context(ctx)?,
                brk: ctrl_break().context(ctx)?,
                close: ctrl_close().context(ctx)?,
                shutdown: ctrl_shutdown().context(ctx)?,
            })
        }
    }

    /// Waits for the next request to stop and returns its name for the log.
    pub async fn recv(&mut self) -> &'static str {
        #[cfg(unix)]
        loop {
            tokio::select! {
                _ = self.int.recv() => return "SIGINT",
                _ = self.term.recv() => return "SIGTERM",
                _ = self.hup.recv() => tracing::info!("SIGHUP ignored"),
            }
        }
        #[cfg(windows)]
        tokio::select! {
            _ = self.c.recv() => "Ctrl-C",
            _ = self.brk.recv() => "Ctrl-Break",
            _ = self.close.recv() => "console close",
            _ = self.shutdown.recv() => "system shutdown",
        }
    }
}
