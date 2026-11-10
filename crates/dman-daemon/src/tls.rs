//! TLS for the daemon's listeners: a rustls `ServerConfig` with the
//! ring provider and a minimal accept loop that serves an axum router over it.
//! `axum::serve` has no TLS support, so this drives hyper directly.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use dman_config::TlsFiles;
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;

/// Handshakes slower than this are dropped.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Installs the ring crypto provider as the process default (idempotent).
pub fn install_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Builds the acceptor for `files` (HTTP/1.1 only: WebSocket upgrades and SSE need it).
///
/// # Errors
/// A readable message when a file is missing or unusable, or key and certificate do not match.
pub fn acceptor(files: &TlsFiles) -> Result<TlsAcceptor, String> {
    let (certs, key) = files.load()?;
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| format!("TLS configuration: {e}"))?
    .with_no_client_auth()
    .with_single_cert(certs, key)
    .map_err(|e| format!("certificate and key do not fit together: {e}"))?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Serves `router` on `listener` until `shutdown` resolves: over TLS when `tls` is set
/// (https/wss only, plaintext clients are rejected at the handshake), plain otherwise.
/// Open connections are asked to finish gracefully.
pub async fn serve(
    listener: TcpListener,
    router: Router,
    tls: Option<TlsAcceptor>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let Some(acceptor) = tls else {
        return axum::serve(listener, router)
            .with_graceful_shutdown(shutdown)
            .await;
    };
    tokio::pin!(shutdown);
    let (close_tx, close_rx) = watch::channel(false);
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => {
                let tcp = match accepted {
                    Ok((tcp, _)) => tcp,
                    Err(e) => {
                        tracing::warn!("accept failed: {e}");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let acceptor = acceptor.clone();
                let service = TowerToHyperService::new(router.clone());
                let mut close = close_rx.clone();
                conns.spawn(async move {
                    let stream = match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await {
                        Ok(Ok(s)) => s,
                        Ok(Err(e)) => {
                            tracing::debug!("TLS handshake failed: {e}");
                            return;
                        }
                        Err(_) => return,
                    };
                    let conn = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .with_upgrades();
                    tokio::pin!(conn);
                    tokio::select! {
                        r = conn.as_mut() => {
                            if let Err(e) = r {
                                tracing::debug!("connection ended: {e}");
                            }
                        }
                        _ = close.changed() => {
                            conn.as_mut().graceful_shutdown();
                            let _ = conn.await;
                        }
                    }
                });
            }
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
        }
    }
    drop(listener);
    let _ = close_tx.send(true);
    while conns.join_next().await.is_some() {}
    Ok(())
}
