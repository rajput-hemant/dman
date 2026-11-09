//! Tests against an in-process fake daemon.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use dman_client::{Client, ClientError, ConnState, ReconnectingClient};
use dman_proto::method::{self, Event, HelloParams, HelloResult, IdParams, ListParams};
use dman_proto::rpc::{Notification, Request, Response};
use dman_proto::{
    API_VERSION, Engine, ErrorCode, GlobalStats, Job, JobId, JobState, Limits, RpcError, SourceKind,
};
use futures_util::{SinkExt, StreamExt};
use secrecy::SecretString;
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message;
use url::Url;

const TOKEN: &str = "s3cret";

struct Fake {
    addr: SocketAddr,
    accepting: Arc<AtomicBool>,
    kill: broadcast::Sender<()>,
    hellos: Arc<AtomicUsize>,
}

impl Fake {
    fn url(&self) -> Url {
        Url::parse(&format!("ws://{}/api/v1/ws", self.addr)).unwrap()
    }
}

fn job(id: u64) -> Job {
    Job {
        id: JobId(id),
        engine: Engine::Aria2,
        kind: SourceKind::Http,
        name: format!("job{id}"),
        state: JobState::Active,
        dir: "/tmp".into(),
        source: "http://example.com/f".into(),
        total_bytes: 100,
        completed_bytes: 10,
        uploaded_bytes: 0,
        download_speed: 0,
        upload_speed: 0,
        connections: 1,
        seeders: None,
        info_hash: None,
        added_at: 0,
        completed_at: None,
        queue_position: None,
        error: None,
    }
}

async fn spawn_fake(api_version: u32) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepting = Arc::new(AtomicBool::new(true));
    let hellos = Arc::new(AtomicUsize::new(0));
    let (kill, _) = broadcast::channel(4);
    let fake = Fake {
        addr,
        accepting: accepting.clone(),
        kill: kill.clone(),
        hellos: hellos.clone(),
    };
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            if !accepting.load(Ordering::SeqCst) {
                drop(tcp);
                continue;
            }
            tokio::spawn(serve(tcp, api_version, kill.subscribe(), hellos.clone()));
        }
    });
    fake
}

async fn serve(
    tcp: tokio::net::TcpStream,
    api_version: u32,
    mut kill: broadcast::Receiver<()>,
    hellos: Arc<AtomicUsize>,
) {
    let Ok(ws) = tokio_tungstenite::accept_async(tcp).await else {
        return;
    };
    let (mut sink, mut stream) = ws.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(s) = out_rx.recv().await {
            if sink.send(Message::text(s)).await.is_err() {
                return;
            }
        }
    });
    let mut authed = false;
    loop {
        let msg = tokio::select! {
            _ = kill.recv() => break,
            m = stream.next() => m,
        };
        let Some(Ok(Message::Text(t))) = msg else {
            break;
        };
        let req: Request = serde_json::from_str(t.as_str()).unwrap();
        let id = req.id.clone().unwrap();
        let reply = |r: Response| out_tx.send(serde_json::to_string(&r).unwrap()).unwrap();
        if req.method == method::HELLO {
            hellos.fetch_add(1, Ordering::SeqCst);
            let p: HelloParams = serde_json::from_value(req.params).unwrap();
            if p.token != TOKEN {
                reply(Response::err(
                    id,
                    RpcError::new(ErrorCode::Unauthorized, "bad token"),
                ));
                continue;
            }
            authed = true;
            let result = HelloResult {
                api_version,
                daemon_version: "0.0.0-test".into(),
                profile: "default".into(),
            };
            reply(Response::ok(id, serde_json::to_value(result).unwrap()));
            continue;
        }
        if !authed {
            reply(Response::err(
                id,
                RpcError::new(ErrorCode::Unauthorized, "hello first"),
            ));
            continue;
        }
        match req.method.as_str() {
            method::SUBSCRIBE => {
                reply(Response::ok(id, Value::Null));
                let events = [
                    Event::Snapshot {
                        jobs: vec![job(1)],
                        stats: GlobalStats::default(),
                        limits: Limits::default(),
                    },
                    Event::JobsUpdated { jobs: vec![job(2)] },
                ];
                for e in events {
                    let n = Notification::new(method::EVENT, e).unwrap();
                    out_tx.send(serde_json::to_string(&n).unwrap()).unwrap();
                }
            }
            method::JOBS_LIST => {
                reply(Response::ok(id, serde_json::to_value([job(1)]).unwrap()));
            }
            method::JOBS_GET => {
                // Answer later requests sooner so replies arrive out of order.
                let p: IdParams = serde_json::from_value(req.params).unwrap();
                let tx = out_tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis((10 - p.id.0) * 15)).await;
                    let r = Response::ok(id, serde_json::to_value(job(p.id.0)).unwrap());
                    let _ = tx.send(serde_json::to_string(&r).unwrap());
                });
            }
            method::STATS_GET => {} // never answered
            _ => reply(Response::err(id, RpcError::not_found(&req.method))),
        }
    }
    writer.abort();
}

fn secret(s: &str) -> SecretString {
    SecretString::from(s.to_owned())
}

async fn wait_state(
    rx: &mut tokio::sync::watch::Receiver<ConnState>,
    f: impl FnMut(&ConnState) -> bool,
) -> ConnState {
    tokio::time::timeout(Duration::from_secs(5), rx.wait_for(f))
        .await
        .expect("timed out waiting for state")
        .unwrap()
        .clone()
}

async fn next_event(rx: &mut broadcast::Receiver<Event>) -> Event {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for event")
        .unwrap()
}

#[tokio::test]
async fn hello_and_list() {
    let fake = spawn_fake(API_VERSION).await;
    let c = Client::connect(&fake.url(), secret(TOKEN), "test/0")
        .await
        .unwrap();
    assert_eq!(c.daemon_version(), "0.0.0-test");
    let jobs = c.list(ListParams::default()).await.unwrap();
    assert_eq!(jobs, vec![job(1)]);
    // Unknown method surfaces the daemon's error.
    let err = c.call::<_, Value>("nope.nope", ()).await.unwrap_err();
    assert!(matches!(&err, ClientError::Rpc(e) if e.code == ErrorCode::NotFound));
    assert_eq!(err.exit_code(), 5);
    assert!(!format!("{c:?}").contains(TOKEN));
}

#[tokio::test]
async fn wrong_token_is_unauthorized() {
    let fake = spawn_fake(API_VERSION).await;
    let err = Client::connect(&fake.url(), secret("wrong"), "test/0")
        .await
        .unwrap_err();
    assert_eq!(err, ClientError::Unauthorized);
    assert_eq!(err.exit_code(), 4);
}

#[tokio::test]
async fn version_mismatch() {
    let fake = spawn_fake(API_VERSION + 1).await;
    let err = Client::connect(&fake.url(), secret(TOKEN), "test/0")
        .await
        .unwrap_err();
    assert_eq!(
        err,
        ClientError::IncompatibleVersion {
            daemon: API_VERSION + 1,
            client: API_VERSION
        }
    );
}

#[tokio::test]
async fn connect_refused_maps_to_unreachable() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    let url = Url::parse(&format!("ws://{addr}/api/v1/ws")).unwrap();
    let err = Client::connect(&url, secret(TOKEN), "t").await.unwrap_err();
    assert!(matches!(err, ClientError::Connect(_)));
    assert_eq!(err.exit_code(), 3);
}

#[tokio::test]
async fn subscribe_delivers_snapshot_then_updates() {
    let fake = spawn_fake(API_VERSION).await;
    let c = Client::connect(&fake.url(), secret(TOKEN), "t")
        .await
        .unwrap();
    let mut rx = c.subscribe().await.unwrap();
    assert!(matches!(next_event(&mut rx).await, Event::Snapshot { jobs, .. } if jobs.len() == 1));
    assert!(
        matches!(next_event(&mut rx).await, Event::JobsUpdated { jobs } if jobs[0].id == JobId(2))
    );
    // A second subscribe does not re-subscribe on the wire.
    let mut rx2 = c.subscribe().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), rx2.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn concurrent_calls_are_correlated() {
    let fake = spawn_fake(API_VERSION).await;
    let c = Client::connect(&fake.url(), secret(TOKEN), "t")
        .await
        .unwrap();
    let handles: Vec<_> = (1..=8u64)
        .map(|i| {
            let c = c.clone();
            tokio::spawn(async move { (i, c.get(JobId(i)).await) })
        })
        .collect();
    for h in handles {
        let (i, res) = h.await.unwrap();
        assert_eq!(res.unwrap().id, JobId(i));
    }
}

#[tokio::test]
async fn unanswered_call_times_out() {
    let fake = spawn_fake(API_VERSION).await;
    let c = Client::connect(&fake.url(), secret(TOKEN), "t")
        .await
        .unwrap()
        .with_timeout(Duration::from_millis(100));
    assert_eq!(c.stats().await.unwrap_err(), ClientError::Timeout);
    // The connection is still usable afterwards.
    assert_eq!(c.get(JobId(9)).await.unwrap().id, JobId(9));
}

#[tokio::test]
async fn reconnecting_client_recovers_and_resnapshots() {
    let fake = spawn_fake(API_VERSION).await;
    let rc = ReconnectingClient::spawn_with_backoff(
        fake.url(),
        secret(TOKEN),
        "t",
        Duration::from_millis(50),
        Duration::from_millis(200),
    );
    let mut state = rc.state();
    let mut events = rc.subscribe();

    wait_state(&mut state, |s| matches!(s, ConnState::Connected { .. })).await;
    assert!(matches!(
        next_event(&mut events).await,
        Event::Snapshot { .. }
    ));
    assert!(matches!(
        next_event(&mut events).await,
        Event::JobsUpdated { .. }
    ));
    assert_eq!(rc.list(ListParams::default()).await.unwrap().len(), 1);

    // Kill the connection and refuse new ones for a while.
    fake.accepting.store(false, Ordering::SeqCst);
    fake.kill.send(()).unwrap();
    wait_state(&mut state, |s| matches!(s, ConnState::Disconnected { .. })).await;
    assert_eq!(
        rc.list(ListParams::default()).await.unwrap_err(),
        ClientError::Disconnected
    );

    fake.accepting.store(true, Ordering::SeqCst);
    let s = wait_state(&mut state, |s| matches!(s, ConnState::Connected { .. })).await;
    assert_eq!(
        s,
        ConnState::Connected {
            daemon_version: "0.0.0-test".into()
        }
    );
    // The same receiver sees the new snapshot.
    assert!(matches!(
        next_event(&mut events).await,
        Event::Snapshot { .. }
    ));
    assert_eq!(rc.get(JobId(3)).await.unwrap().id, JobId(3));
}

#[tokio::test]
async fn unauthorized_is_fatal_for_reconnecting_client() {
    let fake = spawn_fake(API_VERSION).await;
    let rc = ReconnectingClient::spawn_with_backoff(
        fake.url(),
        secret("wrong"),
        "t",
        Duration::from_millis(20),
        Duration::from_millis(40),
    );
    let mut state = rc.state();
    let s = wait_state(&mut state, |s| matches!(s, ConnState::Failed { .. })).await;
    assert_eq!(
        s,
        ConnState::Failed {
            error: ClientError::Unauthorized
        }
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(fake.hellos.load(Ordering::SeqCst), 1, "must not retry");
    assert_eq!(rc.stats().await.unwrap_err(), ClientError::Disconnected);
}
