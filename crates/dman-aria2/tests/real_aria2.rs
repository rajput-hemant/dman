//! Integration test against a real `aria2c`. Run with
//! `DMAN_TEST_ARIA2=1 cargo test -p dman-aria2 -- --ignored`.

use std::net::SocketAddr;
use std::time::Duration;

use dman_aria2::{Aria2Client, DownloadStatus, Options};
use secrecy::SecretString;

#[tokio::test]
#[ignore = "needs aria2c; set DMAN_TEST_ARIA2=1"]
async fn real_aria2_roundtrip() {
    if std::env::var("DMAN_TEST_ARIA2").as_deref() != Ok("1") {
        return;
    }
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let dir = std::env::temp_dir().join(format!("dman-aria2-test-{}-{port}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut child = tokio::process::Command::new("aria2c")
        .args([
            "--enable-rpc",
            &format!("--rpc-listen-port={port}"),
            "--rpc-secret=x",
            &format!("--dir={}", dir.display()),
            "--no-conf",
            "--enable-dht=false",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn aria2c");

    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let mut client = None;
    for _ in 0..100 {
        if let Ok(c) = Aria2Client::connect(addr, SecretString::from("x")).await {
            client = Some(c);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let c = client.expect("aria2c did not come up");

    let v = c.get_version().await.unwrap();
    assert!(v.version.starts_with("1."), "{v:?}");

    let opts = Options::new().with("pause", "true");
    let gid = c
        .add_uri(&["http://127.0.0.1:1/x".to_owned()], &opts, None)
        .await
        .unwrap();
    assert!(gid.is_valid());

    let st = c
        .tell_status(&gid, &["gid", "status", "totalLength", "files"])
        .await
        .unwrap();
    assert_eq!(st.gid.as_ref(), Some(&gid));
    assert_eq!(st.status, Some(DownloadStatus::Paused));
    assert_eq!(st.total_length, Some(0));

    c.change_option(&gid, &Options::new().with("max-download-limit", "100K"))
        .await
        .unwrap();
    let o = c.get_option(&gid).await.unwrap();
    assert_eq!(o.get("max-download-limit"), Some("102400"));

    let stat = c.get_global_stat().await.unwrap();
    assert_eq!(stat.num_waiting, Some(1));

    c.remove(&gid).await.unwrap();
    c.shutdown().await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    let _ = std::fs::remove_dir_all(&dir);
}
