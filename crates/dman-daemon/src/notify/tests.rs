use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::Duration;

use dman_config::{Hook, Notify, NotifyEvent, Webhook};
use dman_proto::JobId;
use dman_proto::method::{Event, Outcome};
use secrecy::SecretString;
use tokio::sync::broadcast;
use wiremock::matchers::{header_exists, method};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::*;

fn payload(outcome: &Outcome) -> Payload {
    Payload::new(
        JobId(7),
        "file.bin",
        "/dl/file.bin",
        outcome,
        None,
        1_700_000_000,
    )
}

fn script(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn hook(script: &Path, timeout_secs: u64) -> Hook {
    Hook {
        on: NotifyEvent::ALL.to_vec(),
        command: vec![script.to_string_lossy().into_owned()],
        timeout_secs,
    }
}

#[test]
fn payload_env_and_body() {
    let p = payload(&Outcome::Failed {
        error: "boom".into(),
    });
    let env: std::collections::HashMap<_, _> = p.env().into_iter().collect();
    assert_eq!(env["DMAN_EVENT"], "failed");
    assert_eq!(env["DMAN_ID"], "7");
    assert_eq!(env["DMAN_PATH"], "/dl/file.bin");
    assert_eq!(env["DMAN_ERROR"], "boom");
    let v = serde_json::to_value(&p).unwrap();
    assert_eq!(v["event"], "failed");
    assert_eq!(v["at"], 1_700_000_000);
    assert_eq!(v["error"], "boom");
    assert!(serde_json::to_value(payload(&Outcome::Completed)).unwrap()["error"].is_null());
}

#[tokio::test]
async fn hook_receives_env_without_a_shell() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("env.txt");
    let s = script(
        dir.path(),
        "hook.sh",
        &format!("env | grep '^DMAN_' | sort > '{}'", out.display()),
    );
    let p = payload(&Outcome::Completed);
    run_hook(&hook(&s, 10), &p.env()).await.unwrap();
    let text = std::fs::read_to_string(out).unwrap();
    assert!(text.contains("DMAN_EVENT=completed"), "{text}");
    assert!(text.contains("DMAN_NAME=file.bin"), "{text}");
    assert!(text.contains("DMAN_ERROR=\n"), "{text}");
}

#[tokio::test]
async fn hook_argv_is_not_shell_expanded() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("args.txt");
    let s = script(
        dir.path(),
        "a.sh",
        &format!("printf '%s' \"$1\" > '{}'", out.display()),
    );
    let mut h = hook(&s, 10);
    h.command.push("$(touch pwned); `x` ; a b".into());
    run_hook(&h, &[]).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(out).unwrap(),
        "$(touch pwned); `x` ; a b"
    );
    assert!(!Path::new("pwned").exists());
}

#[tokio::test]
async fn hook_failure_and_missing_program_are_errors() {
    let dir = tempfile::tempdir().unwrap();
    let s = script(dir.path(), "f.sh", "echo oops >&2; exit 3");
    let e = run_hook(&hook(&s, 10), &[]).await.unwrap_err();
    assert!(e.contains("failed"), "{e}");
    let h = Hook {
        on: vec![NotifyEvent::Completed],
        command: vec!["/nonexistent/dman-hook".into()],
        timeout_secs: 5,
    };
    assert!(
        run_hook(&h, &[])
            .await
            .unwrap_err()
            .contains("cannot start")
    );
}

#[tokio::test]
async fn hook_is_killed_on_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("done");
    let s = script(
        dir.path(),
        "slow.sh",
        &format!("exec sleep 30; touch '{}'", marker.display()),
    );
    let t = std::time::Instant::now();
    let e = run_hook(&hook(&s, 1), &[]).await.unwrap_err();
    assert!(e.contains("timed out"), "{e}");
    assert!(t.elapsed() < Duration::from_secs(10));
    assert!(!marker.exists());
}

#[tokio::test]
async fn hook_output_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let s = script(
        dir.path(),
        "loud.sh",
        "head -c 3000000 /dev/zero | tr '\\0' x",
    );
    run_hook(&hook(&s, 20), &[]).await.unwrap();
}

#[test]
fn signature_matches_known_hmac() {
    // RFC 4231 test case 2.
    assert_eq!(
        signature("Jefe", b"what do ya want for nothing?"),
        "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
}

fn fast() -> Backoff {
    Backoff {
        base: Duration::from_millis(5),
        max: Duration::from_millis(20),
    }
}

fn webhook_cfg(server: &MockServer, retries: u32, secret_ref: Option<&str>) -> Webhook {
    Webhook {
        url: format!("{}/hook", server.uri()),
        timeout_secs: 5,
        retries,
        secret_ref: secret_ref.map(str::to_owned),
    }
}

#[tokio::test]
async fn webhook_retries_on_500_then_succeeds_and_signs() {
    install_crypto_provider();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(2)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(header_exists(webhook::SIGNATURE_HEADER))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let w = WebhookSender::new(
        webhook_cfg(&server, 3, Some("s")),
        Some(SecretString::from("topsecret")),
    )
    .unwrap()
    .with_backoff(fast());
    let body = serde_json::to_vec(&payload(&Outcome::Completed)).unwrap();
    w.send(&body).await.unwrap();
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 3);
    let last: &Request = reqs.last().unwrap();
    assert_eq!(last.body, body);
    assert_eq!(
        last.headers
            .get(webhook::SIGNATURE_HEADER)
            .unwrap()
            .to_str()
            .unwrap(),
        signature("topsecret", &body)
    );
    assert_eq!(
        last.headers.get("content-type").unwrap(),
        "application/json"
    );
}

#[tokio::test]
async fn webhook_gives_up_after_retries_without_leaking() {
    install_crypto_provider();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(3)
        .mount(&server)
        .await;
    let w = WebhookSender::new(webhook_cfg(&server, 2, None), None)
        .unwrap()
        .with_backoff(fast());
    let e = w.send(b"{}").await.unwrap_err();
    assert!(e.contains("3 attempt"), "{e}");
    assert!(!e.contains(&server.uri()), "{e}");
    assert!(
        server.received_requests().await.unwrap()[0]
            .headers
            .get(webhook::SIGNATURE_HEADER)
            .is_none()
    );
}

#[tokio::test]
async fn webhook_unreachable_is_an_error() {
    install_crypto_provider();
    let cfg = Webhook {
        url: "http://127.0.0.1:1/h".into(),
        timeout_secs: 2,
        retries: 1,
        secret_ref: None,
    };
    let w = WebhookSender::new(cfg, None).unwrap().with_backoff(fast());
    assert!(w.send(b"{}").await.is_err());
}

#[tokio::test]
async fn dispatch_applies_filters_and_reports_hook_failures() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("log.txt");
    let ok = script(
        dir.path(),
        "ok.sh",
        &format!("echo \"$DMAN_EVENT\" >> '{}'", out.display()),
    );
    let bad = script(dir.path(), "bad.sh", "exit 1");
    let cfg = Notify {
        desktop: false,
        desktop_on_daemon: false,
        on: vec![NotifyEvent::Completed, NotifyEvent::Failed],
        webhook: None,
        hook: vec![
            Hook {
                on: vec![NotifyEvent::Completed],
                ..hook(&ok, 10)
            },
            Hook {
                on: vec![NotifyEvent::Failed],
                ..hook(&bad, 10)
            },
        ],
    };
    let (tx, mut rx) = broadcast::channel(16);
    let n = Notifier::new(cfg, None, tx).unwrap();
    assert!(!n.is_idle());
    // Filtered out globally, nothing runs.
    let sc = Outcome::SeedCompleted;
    n.dispatch(payload(&sc), &sc);
    let c = Outcome::Completed;
    n.dispatch(payload(&c), &c);
    let f = Outcome::Failed { error: "x".into() };
    n.dispatch(payload(&f), &f);
    let msg = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let Event::Message { text, .. } = msg else {
        panic!("{msg:?}")
    };
    assert!(
        text.contains("hook #2") && text.contains("file.bin"),
        "{text}"
    );
    for _ in 0..50 {
        if out.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(std::fs::read_to_string(out).unwrap(), "completed\n");
}

#[tokio::test]
async fn hooks_run_at_most_four_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let cur = dir.path().join("running");
    std::fs::create_dir(&cur).unwrap();
    let peak = dir.path().join("peak");
    // Each hook registers itself, records the number of registered hooks, waits.
    let s = script(
        dir.path(),
        "c.sh",
        &format!(
            "d='{}'; touch \"$d/$$\"; n=$(ls \"$d\" | wc -l); echo $n >> '{}'; sleep 0.4; rm \"$d/$$\"",
            cur.display(),
            peak.display()
        ),
    );
    let cfg = Notify {
        hook: vec![hook(&s, 20)],
        ..Notify::default()
    };
    let (tx, _rx) = broadcast::channel(16);
    let n = Notifier::new(cfg, None, tx).unwrap();
    let c = Outcome::Completed;
    for _ in 0..8 {
        n.dispatch(payload(&c), &c);
    }
    for _ in 0..100 {
        if std::fs::read_to_string(&peak).map_or(0, |t| t.lines().count()) == 8 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let max = std::fs::read_to_string(&peak)
        .unwrap()
        .lines()
        .map(|l| l.trim().parse::<usize>().unwrap())
        .max()
        .unwrap();
    assert!(max <= MAX_HOOKS, "{max}");
}
