//! The dman daemon. It supervises aria2c, runs yt-dlp for video jobs, owns the job queue
//! and the persistent store, and serves the JSON-RPC API used by every client.
//!
//! Architecture:
//!
//! * [`manager`]: a single actor task owns all job state (records, live
//!   figures, gid index). API handlers send it requests over an mpsc channel
//!   with oneshot replies; it publishes [`dman_proto::method::Event`]s on a
//!   broadcast channel. It polls aria2 every tick (and on aria2
//!   notifications), runs the queue scheduler with the disk guard, and
//!   persists records to the store.
//! * [`search`]: search providers, background searches, search history and the
//!   tracker list; independent of the manager so it can never block downloads.
//! * [`server`]: axum WebSocket endpoint `GET /api/v1/ws` (JSON-RPC 2.0).
//! * `reconcile`: store ↔ aria2 matching on startup and after engine restarts.
//! * [`tick`], [`queue`], [`disk`], [`ops`]: pure decision logic (status
//!   mapping and deltas, scheduling, free space, safe file deletion).
//!
//! A missing aria2 does not stop the daemon: the API keeps working and reports
//! the engine as unavailable.

pub mod budget;
pub mod disk;
pub mod engine;
pub mod history;
pub mod manager;
pub mod notify;
pub mod ops;
pub mod preview;
pub mod queue;
mod reconcile;
pub mod schedule;
pub mod search;
pub mod server;
pub mod tick;
pub mod video;

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use dman_aria2::Aria2Settings;
use dman_config::{Config, DaemonTarget, Paths, Secrets};
use dman_proto::Limits;
use dman_store::Store;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{broadcast, mpsc, watch};

use crate::manager::{Manager, ManagerConfig};
use crate::server::ServerState;

/// Everything the daemon needs to start.
#[derive(Debug, Clone)]
pub struct DaemonOptions {
    /// Filesystem layout (config, data, state directories).
    pub paths: Paths,
    /// The loaded configuration.
    pub config: Config,
    /// Name of the (local) profile to serve.
    pub profile: String,
}

/// Capacity of the event bus; slower subscribers get a fresh snapshot.
const EVENT_CAPACITY: usize = 1024;

/// Takes the single-instance lock of `profile`. The returned file holds the
/// lock until it is dropped.
fn lock(paths: &Paths, profile: &str) -> anyhow::Result<std::fs::File> {
    let path = paths.lock_file(profile);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("cannot open lock file {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            bail!("daemon already running for profile {profile}")
        }
        Err(std::fs::TryLockError::Error(e)) => {
            return Err(e).with_context(|| format!("cannot lock {}", path.display()));
        }
    }
    // Informational only: the pid of the lock holder.
    let _ = file.set_len(0);
    let _ = writeln!(file, "{}", std::process::id());
    Ok(file)
}

/// Runs the daemon in the current process until it is asked to shut down
/// (API `daemon.shutdown`, SIGINT or SIGTERM; SIGHUP is ignored). Returns once
/// everything is flushed and the engine is stopped.
pub async fn run(opts: DaemonOptions) -> anyhow::Result<()> {
    run_with_clock(opts, schedule::system_clock()).await
}

/// Like [`run`] with an explicit time source for the schedule (tests move
/// time with it).
pub async fn run_with_clock(
    opts: DaemonOptions,
    clock: Arc<dyn schedule::Clock>,
) -> anyhow::Result<()> {
    let DaemonOptions {
        paths,
        config,
        profile: name,
    } = opts;
    config.validate().context("invalid configuration")?;
    let (_, profile) = config.profile(Some(&name))?;
    let profile = profile.clone();
    let DaemonTarget::Local { listen, secret_ref } = &profile.daemon else {
        bail!("profile `{name}` points to a remote daemon; it cannot be served here");
    };
    for d in [paths.state_dir(), paths.data_dir()] {
        std::fs::create_dir_all(d).with_context(|| format!("cannot create {}", d.display()))?;
    }
    let lock_file = lock(&paths, &name)?;

    let mut secrets = Secrets::load(&paths)?;
    let token = secrets.ensure(secret_ref)?;
    let aria2_secret = secrets.ensure(&format!("aria2-{name}"))?;

    let store_path = paths.data_dir().join(format!("{name}.redb"));
    let store = tokio::task::spawn_blocking(move || Store::open(store_path))
        .await?
        .context("cannot open the job store")?;

    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("cannot listen on {listen}"))?;
    tracing::info!(profile = %name, %listen, "dman daemon starting");

    let mut sigint = signal(SignalKind::interrupt()).context("cannot install signal handlers")?;
    let mut sigterm = signal(SignalKind::terminate()).context("cannot install signal handlers")?;
    let mut sighup = signal(SignalKind::hangup()).context("cannot install signal handlers")?;

    let make_settings: engine::SettingsFn = {
        let paths = paths.clone();
        let name = name.clone();
        let config = config.clone();
        let profile = profile.clone();
        Box::new(move || {
            // Re-read the config file so a fixed `aria2.path` is picked up on
            // restart; keep the startup config when there is no readable file.
            let cfg = if paths.config_file().exists() {
                Config::load(&paths).unwrap_or_else(|_| config.clone())
            } else {
                config.clone()
            };
            Aria2Settings {
                binary: dman_config::expand_tilde(&cfg.aria2.path),
                rpc_port: cfg.aria2.rpc_port,
                secret: aria2_secret.clone(),
                session_file: paths.aria2_session_file(&name),
                conf_file: aria2_conf_path(&paths, &name),
                log_file: Some(paths.state_dir().join(format!("aria2-{name}.log"))),
                download_dir: profile.download_path(),
                extra_args: cfg.aria2.extra_args.clone(),
                detach_seed_only: true,
            }
        })
    };
    let (engine, engine_rx, engine_task) = engine::spawn(make_settings, engine::RETRY_EVERY).await;

    let video = Arc::new(video::VideoService::start(&config.video, Some(paths.clone())).await);
    let previews = preview::PreviewRegistry::new(paths.state_dir().join("metadata"));
    previews.reset_dir();
    let preview = Arc::new(preview::Preview::new(previews.clone(), engine_rx.clone()));
    let (events, _) = broadcast::channel(EVENT_CAPACITY);
    let search = search::SearchService::new(
        paths.clone(),
        store.clone(),
        events.clone(),
        &config,
        &secrets,
    )
    .context("cannot set up search")?;
    let tracker_task = search.spawn_tracker_refresher();
    let notifier = {
        let secret = match config
            .notify
            .webhook
            .as_ref()
            .and_then(|w| w.secret_ref.as_ref())
        {
            Some(r) => Some(secrets.get(r).with_context(|| {
                format!("notify.webhook.secret_ref `{r}` is not defined in secrets.toml")
            })?),
            None => None,
        };
        notify::Notifier::new(config.notify.clone(), secret, events.clone())?
    };
    let notifier_rx = events.subscribe();
    let (manager, manager_task) = Manager::spawn(ManagerConfig {
        store,
        profile: profile.clone(),
        limits: Limits {
            download: config.limits.download.0,
            upload: config.limits.upload.0,
            max_concurrent: config.limits.max_concurrent,
        },
        schedule: config.schedule.clone(),
        clock,
        config_paths: Some(paths.clone()),
        tick: Duration::from_millis(config.aria2.tick_ms.max(50)),
        events: events.clone(),
        engine: engine_rx,
        previews,
        trackers: search.trackers(),
        video: config.video.clone(),
        video_tools: video.subscribe(),
    });

    let notifier_task =
        (!notifier.is_idle()).then(|| tokio::spawn(notifier.run(notifier_rx, manager.clone())));

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (request_tx, mut request_rx) = mpsc::channel::<()>(1);
    let search_close = search.clone();
    let state = Arc::new(ServerState {
        manager: manager.clone(),
        token,
        profile: name.clone(),
        events,
        shutdown: shutdown_rx.clone(),
        shutdown_request: request_tx,
        engine: engine.clone(),
        preview,
        search,
        video,
    });
    let mut graceful = shutdown_rx.clone();
    let mut server = tokio::spawn(async move {
        axum::serve(listener, server::router(state))
            .with_graceful_shutdown(async move {
                let _ = graceful.wait_for(|v| *v).await;
            })
            .await
    });

    let mut server_done = false;
    loop {
        tokio::select! {
            _ = sigint.recv() => { tracing::info!("SIGINT received"); break; }
            _ = sigterm.recv() => { tracing::info!("SIGTERM received"); break; }
            _ = sighup.recv() => tracing::info!("SIGHUP ignored"),
            _ = request_rx.recv() => break,
            r = &mut server => {
                server_done = true;
                match r {
                    Ok(Ok(())) => tracing::error!("API server stopped unexpectedly"),
                    Ok(Err(e)) => tracing::error!("API server failed: {e}"),
                    Err(e) => tracing::error!("API server task failed: {e}"),
                }
                break;
            }
        }
    }

    tracing::info!("shutting down");
    let _ = shutdown_tx.send(true);
    manager.shutdown().await;
    let _ = manager_task.await;
    if let Some(t) = notifier_task {
        t.abort();
    }
    if let Some(t) = tracker_task {
        t.abort();
    }
    search_close.close();
    engine.shutdown().await;
    let _ = engine_task.await;
    if !server_done
        && tokio::time::timeout(Duration::from_secs(5), &mut server)
            .await
            .is_err()
    {
        server.abort();
    }
    let _ = lock_file.unlock();
    drop(lock_file);
    tracing::info!("daemon stopped");
    Ok(())
}

/// Path of the aria2 conf file the daemon generates for `profile`.
pub fn aria2_conf_path(paths: &Paths, profile: &str) -> PathBuf {
    paths.state_dir().join(format!("aria2-{profile}.conf"))
}
