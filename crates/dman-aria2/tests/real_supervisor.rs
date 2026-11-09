//! Supervisor and option-encoding tests against a real `aria2c`. Run with
//! `DMAN_TEST_ARIA2=1 cargo test -p dman-aria2 -- --ignored --test-threads=1`.

use std::path::PathBuf;
use std::time::Duration;

use dman_aria2::{
    Aria2Client, Aria2Settings, EngineState, Options, Supervisor, SupervisorError,
    job_options_to_aria2,
};
use dman_proto::JobOptions;
use secrecy::SecretString;
use serde_json::json;

fn enabled() -> bool {
    std::env::var("DMAN_TEST_ARIA2").as_deref() == Ok("1")
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

fn settings(name: &str) -> (Aria2Settings, PathBuf) {
    let dir = std::env::temp_dir().join(format!("dman-real-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let s = Aria2Settings::new(
        free_port(),
        SecretString::from("test-secret"),
        dir.join("state/session"),
        dir.join("state/aria2.conf"),
        dir.join("dl"),
    );
    (s, dir)
}

async fn wait_state(
    rx: &mut tokio::sync::watch::Receiver<EngineState>,
    f: impl Fn(&EngineState) -> bool,
    secs: u64,
) -> EngineState {
    tokio::time::timeout(Duration::from_secs(secs), rx.wait_for(|s| f(s)))
        .await
        .expect("timed out waiting for state")
        .expect("state channel closed")
        .clone()
}

fn pid_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
        && std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .map(|s| !s.contains(") Z "))
            .unwrap_or(false)
}

async fn wait_gone(pid: u32) {
    for _ in 0..50 {
        if !pid_alive(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("process {pid} still alive");
}

#[tokio::test]
#[ignore = "needs aria2c; set DMAN_TEST_ARIA2=1"]
async fn start_attach_restart_shutdown() {
    if !enabled() {
        return;
    }
    let (s, dir) = settings("life");
    let sup = Supervisor::start(s.clone()).await.expect("start");
    let EngineState::Running { info, attached, .. } = sup.state().borrow().clone() else {
        panic!("not running");
    };
    assert!(!attached);
    assert!(info.version.starts_with("1."), "{info:?}");
    assert!(info.caps.https);
    let pid = sup.pid().expect("pid");
    assert!(pid_alive(pid));
    sup.client().unwrap().get_version().await.unwrap();

    // The secret is not on the command line.
    let cmdline = std::fs::read_to_string(format!("/proc/{pid}/cmdline")).unwrap();
    assert!(!cmdline.contains("test-secret"), "{cmdline}");

    // A second supervisor attaches instead of spawning.
    let second = Supervisor::start(s.clone()).await.expect("attach");
    assert!(second.is_attached());
    assert_eq!(second.pid(), None);
    // Wrong secret: port occupied, nothing spawned.
    let mut wrong = s.clone();
    wrong.secret = SecretString::from("nope");
    match Supervisor::start(wrong).await {
        Err(SupervisorError::PortOccupied { .. }) => {}
        other => panic!("expected PortOccupied, got {other:?}"),
    }
    // Detaching from an engine we do not own leaves it running.
    second.shutdown(false).await;
    sup.client().unwrap().get_version().await.unwrap();

    // Kill -9: Restarting, then Running again with a new pid.
    let mut rx = sup.state();
    let status = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    wait_state(
        &mut rx,
        |st| matches!(st, EngineState::Restarting { .. }),
        10,
    )
    .await;
    wait_state(&mut rx, |st| matches!(st, EngineState::Running { .. }), 20).await;
    let new_pid = sup.pid().expect("new pid");
    assert_ne!(new_pid, pid);
    sup.client().unwrap().get_version().await.unwrap();

    sup.shutdown(true).await;
    wait_gone(new_pid).await;
    assert!(
        Aria2Client::connect(
            ([127, 0, 0, 1], s.rpc_port).into(),
            SecretString::from("test-secret")
        )
        .await
        .is_err()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
#[ignore = "needs aria2c; set DMAN_TEST_ARIA2=1"]
async fn attached_engine_stopped_on_request() {
    if !enabled() {
        return;
    }
    let (s, dir) = settings("attach-stop");
    let owner = Supervisor::start(s.clone()).await.expect("start");
    let pid = owner.pid().unwrap();
    let attached = Supervisor::start(s.clone()).await.expect("attach");
    assert!(attached.is_attached());
    attached.shutdown(true).await;
    wait_gone(pid).await;
    drop(owner);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Two-file torrent (`a`, `b`, 5 bytes each) for option tests.
fn torrent() -> Vec<u8> {
    let mut t = b"d4:infod5:filesld6:lengthi5e4:pathl1:aeed6:lengthi5e4:pathl1:beee4:name3:tor12:piece lengthi16384e6:pieces20:"
        .to_vec();
    t.extend([7u8; 20]);
    t.extend(b"ee");
    t
}

/// Experiment backing the module docs of `options.rs`: both a JSON array and
/// a newline-joined string make `getOption` show ALL index-out / header values.
#[tokio::test]
#[ignore = "needs aria2c; set DMAN_TEST_ARIA2=1"]
async fn multi_value_options_array_and_newline_forms() {
    if !enabled() {
        return;
    }
    let (s, dir) = settings("multi");
    let sup = Supervisor::start(s).await.expect("start");
    let c = sup.client().unwrap();

    // Newline-joined string, produced by job_options_to_aria2.
    let mut jo = JobOptions::default();
    jo.file_names.insert(1, "one".into());
    jo.file_names.insert(2, "two".into());
    jo.headers = vec!["X-A: 1".into(), "X-B: 2".into()];
    let mut opts = job_options_to_aria2(&jo).unwrap();
    opts.set("pause", "true");
    let gid = c.add_torrent(&torrent(), &[], &opts, None).await.unwrap();
    let got = c.get_option(&gid).await.unwrap();
    assert_eq!(got.get("index-out").unwrap().trim_end(), "1=one\n2=two");
    assert_eq!(got.get("header").unwrap().trim_end(), "X-A: 1\nX-B: 2");

    // JSON array form (raw call; Options cannot hold arrays).
    let gid2: dman_aria2::Gid = c
        .call(
            "aria2.addTorrent",
            vec![
                json!(base64_encode(&torrent())),
                json!([]),
                json!({"pause": "true", "index-out": ["1=x", "2=y"], "header": ["H: 1", "G: 2"]}),
            ],
        )
        .await
        .unwrap();
    let got = c.get_option(&gid2).await.unwrap();
    assert_eq!(got.get("index-out").unwrap().trim_end(), "1=x\n2=y");
    assert_eq!(got.get("header").unwrap().trim_end(), "H: 1\nG: 2");

    // changeOption on a paused torrent with the newline form.
    c.change_option(&gid, &Options::new().with("index-out", "1=uno\n2=dos"))
        .await
        .unwrap();
    let got = c.get_option(&gid).await.unwrap();
    assert_eq!(got.get("index-out").unwrap().trim_end(), "1=uno\n2=dos");

    sup.shutdown(true).await;
    let _ = std::fs::remove_dir_all(&dir);
}

fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for ch in data.chunks(3) {
        let n = (u32::from(ch[0]) << 16)
            | (u32::from(*ch.get(1).unwrap_or(&0)) << 8)
            | u32::from(*ch.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= ch.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
