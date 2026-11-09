//! Client tests against an in-process fake aria2 server.

use std::net::SocketAddr;
use std::time::Duration;

use dman_aria2::{Aria2Client, Aria2Event, DownloadStatus, Error, Gid, Options};
use futures_util::{SinkExt, StreamExt};
use secrecy::SecretString;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

const SECRET: &str = "s3cret";

/// Spawn a fake server; `batch` > 0 buffers that many tellStatus requests and
/// answers them in reverse order.
async fn spawn_server(batch: usize) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(handle(stream, batch));
        }
    });
    addr
}

fn status_for(gid: &str) -> Value {
    json!({
        "gid": gid, "status": "active", "totalLength": "12345",
        "completedLength": "100", "downloadSpeed": "2048", "connections": "4",
        "seeder": "false", "errorCode": "0",
        "files": [{"index": "1", "path": "/x", "length": "12345",
                   "completedLength": "100", "selected": "true",
                   "uris": [{"uri": "http://a/b", "status": "used"}]}],
    })
}

async fn handle(stream: tokio::net::TcpStream, batch: usize) {
    let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    let (mut tx, mut rx) = ws.split();
    let mut buffered: Vec<Value> = Vec::new();
    while let Some(Ok(msg)) = rx.next().await {
        let Message::Text(t) = msg else { continue };
        let req: Value = serde_json::from_str(t.as_str()).expect("json");
        let id = req["id"].clone();
        let method = req["method"].as_str().unwrap_or("").to_owned();
        let params = req["params"].as_array().cloned().unwrap_or_default();
        let reply = |v: Value| json!({"jsonrpc": "2.0", "id": id, "result": v});
        let out = if method == "system.multicall" {
            let calls = params[0].as_array().cloned().unwrap_or_default();
            let res: Vec<Value> = calls
                .iter()
                .map(|c| {
                    let p = c["params"].as_array().cloned().unwrap_or_default();
                    if p.first().and_then(Value::as_str) != Some(&format!("token:{SECRET}")) {
                        return json!({"code": 1, "message": "Unauthorized"});
                    }
                    match c["methodName"].as_str() {
                        Some("aria2.getVersion") => {
                            json!([{"version": "1.37.0", "enabledFeatures": []}])
                        }
                        _ => json!({"code": 1, "message": "nope"}),
                    }
                })
                .collect();
            reply(json!(res))
        } else if params.first().and_then(Value::as_str) != Some(&format!("token:{SECRET}")) {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": 1, "message": "Unauthorized"}})
        } else {
            match method.as_str() {
                "aria2.getVersion" => {
                    reply(json!({"version": "1.37.0", "enabledFeatures": ["BitTorrent"]}))
                }
                "aria2.getGlobalStat" => reply(json!({
                    "downloadSpeed": "10", "uploadSpeed": "0", "numActive": "1",
                    "numWaiting": "2", "numStopped": "3", "numStoppedTotal": "4"})),
                "aria2.tellStatus" => {
                    let gid = params[1].as_str().unwrap_or("").to_owned();
                    let r = reply(status_for(&gid));
                    if batch > 0 {
                        buffered.push(r);
                        if buffered.len() < batch {
                            continue;
                        }
                        for r in buffered.drain(..).rev() {
                            tx.send(Message::text(r.to_string())).await.ok();
                        }
                        continue;
                    }
                    r
                }
                "aria2.pause" => {
                    let note = json!({"jsonrpc": "2.0", "method": "aria2.onDownloadPause",
                                      "params": [{"gid": params[1]}]});
                    tx.send(Message::text(note.to_string())).await.ok();
                    reply(params[1].clone())
                }
                "test.hang" => continue,
                "test.close" => {
                    tx.close().await.ok();
                    return;
                }
                _ => json!({"jsonrpc": "2.0", "id": id,
                            "error": {"code": 1, "message": "unknown method"}}),
            }
        };
        if tx.send(Message::text(out.to_string())).await.is_err() {
            return;
        }
    }
}

async fn client(addr: SocketAddr) -> Aria2Client {
    Aria2Client::connect(addr, SecretString::from(SECRET))
        .await
        .expect("connect")
}

#[tokio::test]
async fn parses_numeric_strings() {
    let c = client(spawn_server(0).await).await;
    let st = c
        .tell_status(&Gid::from("0123456789abcdef"), &[])
        .await
        .unwrap();
    assert_eq!(st.status, Some(DownloadStatus::Active));
    assert_eq!(st.total_length, Some(12345));
    assert_eq!(st.download_speed, Some(2048));
    assert_eq!(st.connections, Some(4));
    assert_eq!(st.seeder, Some(false));
    let f = &st.files.unwrap()[0];
    assert_eq!(f.index, Some(1));
    assert_eq!(f.selected, Some(true));
    assert_eq!(f.uris[0].status.as_deref(), Some("used"));

    let g = c.get_global_stat().await.unwrap();
    assert_eq!(g.num_stopped_total, Some(4));
    let v = c.get_version().await.unwrap();
    assert_eq!(v.version, "1.37.0");
    assert_eq!(v.enabled_features, ["BitTorrent"]);
}

#[tokio::test]
async fn wrong_token_is_rpc_error() {
    let addr = spawn_server(0).await;
    let c = Aria2Client::connect(addr, SecretString::from("bad"))
        .await
        .unwrap();
    match c.get_version().await {
        Err(Error::Rpc { code: 1, message }) => assert_eq!(message, "Unauthorized"),
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn concurrent_calls_correlate_out_of_order() {
    let c = client(spawn_server(20).await).await;
    let mut set = tokio::task::JoinSet::new();
    for i in 0..20u32 {
        let c = c.clone();
        set.spawn(async move {
            let gid = Gid::new(format!("{i:016x}"));
            let st = c.tell_status(&gid, &["gid"]).await.unwrap();
            (gid, st.gid.unwrap())
        });
    }
    let mut n = 0;
    while let Some(r) = set.join_next().await {
        let (want, got) = r.unwrap();
        assert_eq!(want, got);
        n += 1;
    }
    assert_eq!(n, 20);
}

#[tokio::test]
async fn call_times_out_and_client_stays_usable() {
    let c = client(spawn_server(0).await)
        .await
        .with_timeout(Duration::from_millis(150));
    let r = c.call::<Value>("test.hang", vec![]).await;
    assert!(matches!(r, Err(Error::Timeout)), "{r:?}");
    assert!(c.is_connected());
    assert_eq!(c.get_version().await.unwrap().version, "1.37.0");
}

#[tokio::test]
async fn disconnect_fails_pending_and_future_calls() {
    let c = client(spawn_server(0).await).await;
    let hang = {
        let c = c.clone();
        tokio::spawn(async move { c.call::<Value>("test.hang", vec![]).await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    let r = c.call::<Value>("test.close", vec![]).await;
    assert!(matches!(r, Err(Error::Disconnected)), "{r:?}");
    assert!(matches!(hang.await.unwrap(), Err(Error::Disconnected)));
    tokio::time::timeout(Duration::from_secs(2), c.closed())
        .await
        .expect("closed() resolves");
    assert!(!c.is_connected());
    assert!(matches!(c.get_version().await, Err(Error::Disconnected)));
}

#[tokio::test]
async fn notifications_are_delivered() {
    let c = client(spawn_server(0).await).await;
    let mut rx = c.subscribe();
    let gid = Gid::from("00000000000000aa");
    c.pause(&gid).await.unwrap();
    let n = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(n.event, Aria2Event::DownloadPause);
    assert_eq!(n.gid, gid);
}

#[tokio::test]
async fn multicall_mixes_results_and_errors() {
    let c = client(spawn_server(0).await).await;
    let res = c
        .multicall(vec![
            ("aria2.getVersion".into(), vec![]),
            ("aria2.bogus".into(), vec![]),
        ])
        .await
        .unwrap();
    assert_eq!(res[0].as_ref().unwrap()["version"], "1.37.0");
    assert_eq!(res[1].as_ref().unwrap_err().message, "nope");
}

#[tokio::test]
async fn connect_failure_is_connect_error() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    let r = Aria2Client::connect(addr, SecretString::from("x")).await;
    assert!(matches!(r, Err(Error::Connect(_))));
    let _ = Options::new();
}
