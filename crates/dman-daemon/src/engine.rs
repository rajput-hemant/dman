//! Owner of the aria2 supervisor: starts it, forwards its state to the job
//! manager, restarts it on request (`engine.restart`) and retries starting it
//! in the background while it is unavailable (e.g. `aria2c` was missing at
//! daemon start).

use std::time::Duration;

use dman_aria2::{Aria2Settings, EngineState, Supervisor};
use dman_proto::method::EngineStatus;
use dman_proto::{Engine, ErrorCode, RpcError};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

/// How often a missing engine is looked for again.
pub const RETRY_EVERY: Duration = Duration::from_secs(60);

enum Cmd {
    Restart(oneshot::Sender<Result<EngineStatus, RpcError>>),
    Shutdown(oneshot::Sender<()>),
}

/// Cloneable handle to the engine task.
#[derive(Clone, Debug)]
pub struct EngineHandle {
    tx: mpsc::Sender<Cmd>,
}

fn gone() -> RpcError {
    RpcError::new(ErrorCode::General, "the daemon is shutting down")
}

impl EngineHandle {
    /// Starts the engine again (restarting it cleanly if it is running).
    /// `EngineUnavailable` when it cannot be started.
    pub async fn restart(&self) -> Result<EngineStatus, RpcError> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(Cmd::Restart(tx)).await.map_err(|_| gone())?;
        rx.await.map_err(|_| gone())?
    }

    /// Stops supervising and shuts the engine down; returns when done.
    pub async fn shutdown(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Cmd::Shutdown(tx)).await.is_ok() {
            let _ = rx.await;
        }
    }
}

/// Builds the settings for each start attempt (so a changed `aria2.path` in
/// the config file is picked up).
pub type SettingsFn = Box<dyn Fn() -> Aria2Settings + Send + Sync>;

/// Tries a first start (a failure is not fatal) and spawns the engine task.
/// The receiver carries the engine state for the job manager.
pub async fn spawn(
    make: SettingsFn,
    retry: Duration,
) -> (EngineHandle, watch::Receiver<EngineState>, JoinHandle<()>) {
    let mut last_reason = None;
    let (sup, initial) = match Supervisor::start(make()).await {
        Ok(s) => {
            let st = s.state().borrow().clone();
            (Some(s), st)
        }
        Err(e) => {
            tracing::warn!("aria2 unavailable, serving without it: {e}");
            last_reason = Some(e.to_string());
            (
                None,
                EngineState::Failed {
                    reason: e.to_string(),
                },
            )
        }
    };
    let (state_tx, state_rx) = watch::channel(initial);
    let (tx, rx) = mpsc::channel(4);
    let task = tokio::spawn(run(make, sup, state_tx, rx, retry, last_reason));
    (EngineHandle { tx }, state_rx, task)
}

fn status_of(state: &EngineState) -> Result<EngineStatus, RpcError> {
    match state {
        EngineState::Running { info, .. } => Ok(EngineStatus {
            engine: Engine::Aria2,
            available: true,
            version: Some(info.version.clone()),
            features: info.features.clone(),
            reason: None,
        }),
        EngineState::Failed { reason } | EngineState::Restarting { reason, .. } => Err(
            RpcError::new(ErrorCode::EngineUnavailable, format!("aria2: {reason}")),
        ),
        _ => Err(RpcError::new(
            ErrorCode::EngineUnavailable,
            "aria2 is not running",
        )),
    }
}

async fn run(
    make: SettingsFn,
    mut sup: Option<Supervisor>,
    state_tx: watch::Sender<EngineState>,
    mut rx: mpsc::Receiver<Cmd>,
    retry: Duration,
    mut last_reason: Option<String>,
) {
    let mut sup_rx = sup.as_ref().map(Supervisor::state);
    let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + retry, retry);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            cmd = rx.recv() => match cmd {
                None => {
                    if let Some(s) = sup.take() {
                        s.shutdown(true).await;
                    }
                    return;
                }
                Some(Cmd::Shutdown(done)) => {
                    if let Some(s) = sup.take() {
                        s.shutdown(true).await;
                    }
                    let _ = done.send(());
                    return;
                }
                Some(Cmd::Restart(reply)) => {
                    if let Some(s) = sup.take() {
                        sup_rx = None;
                        state_tx.send_replace(EngineState::Starting);
                        s.shutdown(true).await;
                    }
                    let res = match Supervisor::start(make()).await {
                        Ok(s) => {
                            let st = s.state().borrow().clone();
                            sup_rx = Some(s.state());
                            sup = Some(s);
                            last_reason = None;
                            state_tx.send_replace(st.clone());
                            status_of(&st)
                        }
                        Err(e) => {
                            let reason = e.to_string();
                            last_reason = Some(reason.clone());
                            state_tx.send_replace(EngineState::Failed { reason: reason.clone() });
                            Err(RpcError::new(
                                ErrorCode::EngineUnavailable,
                                format!("aria2 cannot be started: {reason}"),
                            ))
                        }
                    };
                    let _ = reply.send(res);
                }
            },
            changed = async {
                match sup_rx.as_mut() {
                    Some(r) => {
                        if r.changed().await.is_err() {
                            std::future::pending::<()>().await;
                        }
                    }
                    None => std::future::pending::<()>().await,
                }
            } => {
                let () = changed;
                if let Some(r) = sup_rx.as_mut() {
                    let st = r.borrow_and_update().clone();
                    state_tx.send_replace(st);
                }
            }
            _ = timer.tick() => {
                let down = match &sup_rx {
                    None => true,
                    Some(r) => matches!(
                        &*r.borrow(),
                        EngineState::Failed { .. } | EngineState::Stopped
                    ),
                };
                if !down {
                    continue;
                }
                // Cheap: a failed attempt is a binary lookup / spawn error.
                match Supervisor::start(make()).await {
                    Ok(s) => {
                        tracing::info!("aria2 became available; started it");
                        let st = s.state().borrow().clone();
                        sup_rx = Some(s.state());
                        sup = Some(s);
                        last_reason = None;
                        state_tx.send_replace(st);
                    }
                    Err(e) => {
                        let reason = e.to_string();
                        tracing::debug!("aria2 still unavailable: {reason}");
                        if last_reason.as_deref() != Some(reason.as_str()) {
                            last_reason = Some(reason.clone());
                            sup = None;
                            sup_rx = None;
                            state_tx.send_replace(EngineState::Failed { reason });
                        }
                    }
                }
            }
        }
    }
}
