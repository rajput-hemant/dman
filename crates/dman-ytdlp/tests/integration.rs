//! Integration tests against the real yt-dlp/ffmpeg, served from a local range-capable HTTP
//! server (generic extractor). Run with `DMAN_TEST_YTDLP=1 cargo test -p dman-ytdlp -- --include-ignored`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::Duration;

use dman_ytdlp::{
    DownloadSpec, Outcome, PlanKind, ProbeOptions, ProbeResult, QualityPreset, Tools, VideoInfo,
    fallback_plan, probe, spawn,
};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use url::Url;

macro_rules! gate {
    () => {
        if std::env::var("DMAN_TEST_YTDLP").as_deref() != Ok("1") {
            eprintln!("DMAN_TEST_YTDLP != 1, skipping");
            return;
        }
    };
}

/// Minimal static file server with Range support. Aborted when the guard is dropped.
struct Server {
    addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    async fn start(root: PathBuf) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                let root = root.clone();
                tokio::spawn(async move {
                    let _ = handle(sock, &root).await;
                });
            }
        });
        Server { addr, task }
    }

    fn url(&self, path: &str) -> Url {
        Url::parse(&format!("http://{}/{path}", self.addr)).unwrap()
    }
}

async fn handle(sock: TcpStream, root: &Path) -> std::io::Result<()> {
    use tokio::io::AsyncBufReadExt;
    let (rd, mut wr) = sock.into_split();
    let mut rd = BufReader::new(rd);
    // One request per connection (Connection: close).
    {
        let mut line = String::new();
        if rd.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let path = parts
            .next()
            .unwrap_or("/")
            .trim_start_matches('/')
            .to_string();
        let mut range = None;
        loop {
            let mut h = String::new();
            rd.read_line(&mut h).await?;
            let h = h.trim();
            if h.is_empty() {
                break;
            }
            if let Some(v) = h
                .to_ascii_lowercase()
                .strip_prefix("range: bytes=")
                .map(str::to_string)
            {
                range = Some(v);
            }
        }
        let file = root.join(path.split('?').next().unwrap());
        let Ok(mut f) = tokio::fs::File::open(&file).await else {
            wr.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await?;
            return Ok(());
        };
        let len = f.metadata().await?.len();
        let (start, end) = match range.as_deref().and_then(|r| r.split_once('-')) {
            Some((s, e)) => {
                let s: u64 = s.parse().unwrap_or(0);
                let e: u64 = e.parse().unwrap_or(len - 1).min(len - 1);
                (s, e)
            }
            None => (0, len.saturating_sub(1)),
        };
        let ctype = match file.extension().and_then(|e| e.to_str()) {
            Some("mp4") => "video/mp4",
            Some("mpd") => "application/dash+xml",
            Some("m4s") => "video/iso.segment",
            _ => "application/octet-stream",
        };
        let n = end + 1 - start;
        let head = if range.is_some() {
            format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{len}\r\n")
        } else {
            "HTTP/1.1 200 OK\r\n".to_string()
        };
        wr.write_all(
            format!(
                "{head}Content-Type: {ctype}\r\nContent-Length: {n}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await?;
        if method != "HEAD" {
            f.seek(std::io::SeekFrom::Start(start)).await?;
            let mut left = n;
            let mut buf = vec![0u8; 64 * 1024];
            while left > 0 {
                let take = (left as usize).min(buf.len());
                let k = f.read(&mut buf[..take]).await?;
                if k == 0 {
                    break;
                }
                wr.write_all(&buf[..k]).await?;
                left -= k as u64;
            }
        }
        wr.flush().await?;
        Ok(())
    }
}

fn ffmpeg(args: &[&str], cwd: &Path) {
    let st = StdCommand::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("ffmpeg");
    assert!(st.success());
}

/// 10 s 640x360 test video (about 2 MB) as `test.mp4`, plus a DASH manifest with separate
/// audio/video adaptation sets under `dash/`.
fn make_media() -> TempDir {
    let d = tempfile::tempdir().unwrap();
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=25",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440",
            "-t",
            "10",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-b:v",
            "1500k",
            "-c:a",
            "aac",
            "test.mp4",
        ],
        d.path(),
    );
    std::fs::create_dir(d.path().join("dash")).unwrap();
    ffmpeg(
        &[
            "-i",
            "test.mp4",
            "-map",
            "0:v",
            "-map",
            "0:a",
            "-c",
            "copy",
            "-f",
            "dash",
            "-seg_duration",
            "2",
            "-use_template",
            "1",
            "-use_timeline",
            "1",
            "-adaptation_sets",
            "id=0,streams=v id=1,streams=a",
            "dash/manifest.mpd",
        ],
        d.path(),
    );
    // Two video representations (360p and 180p) and one audio: a quality ladder.
    std::fs::create_dir(d.path().join("ladder")).unwrap();
    ffmpeg(
        &[
            "-i",
            "test.mp4",
            "-filter_complex",
            "[0:v]split=2[a][b];[b]scale=320:180[bs]",
            "-map",
            "[a]",
            "-map",
            "[bs]",
            "-map",
            "0:a",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-c:a",
            "aac",
            "-f",
            "dash",
            "-seg_duration",
            "2",
            "-use_template",
            "1",
            "-use_timeline",
            "1",
            "-adaptation_sets",
            "id=0,streams=v id=1,streams=a",
            "ladder/manifest.mpd",
        ],
        d.path(),
    );
    d
}

async fn tools() -> Tools {
    let t = Tools::detect(None, None).await;
    assert!(t.ytdlp.is_some(), "yt-dlp not found");
    assert!(t.ffmpeg.is_some(), "ffmpeg not found");
    t
}

async fn probe_video(t: &Tools, url: &Url) -> VideoInfo {
    match probe(t, url, &ProbeOptions::default()).await.unwrap() {
        ProbeResult::Video(v) => v,
        other => panic!("expected video, got {other:?}"),
    }
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[tokio::test]
#[ignore = "needs yt-dlp and ffmpeg; set DMAN_TEST_YTDLP=1"]
async fn probe_direct_file() {
    gate!();
    let media = make_media();
    let srv = Server::start(media.path().to_path_buf()).await;
    let t = tools().await;
    let v = probe_video(&t, &srv.url("test.mp4")).await;
    assert_eq!(v.formats.len(), 1, "{v:?}");
    let missing = probe(&t, &srv.url("nope.mp4"), &ProbeOptions::default()).await;
    assert!(missing.is_err());
}

/// The quality picker's choices from real probes: a direct file needs no
/// choice; a DASH ladder offers merged choices, and the chosen `VID+AUD` id
/// downloads into one file.
#[tokio::test]
#[ignore = "needs yt-dlp and ffmpeg; set DMAN_TEST_YTDLP=1"]
async fn probe_choices_for_direct_file_and_dash_ladder() {
    gate!();
    use dman_ytdlp::{AudioPref, ChoiceDefaults, ChoiceKind, build_choices};
    let media = make_media();
    let srv = Server::start(media.path().to_path_buf()).await;
    let t = tools().await;
    let d = ChoiceDefaults {
        preset: QualityPreset::Best,
        audio: AudioPref::Best,
    };

    let direct = build_choices(&probe_video(&t, &srv.url("test.mp4")).await, &t, d);
    assert!(!direct.needs_choice, "{direct:?}");
    assert_eq!(direct.video.len(), 1);
    assert_eq!(direct.video[0].kind, ChoiceKind::Muxed);

    let v = probe_video(&t, &srv.url("ladder/manifest.mpd")).await;
    let c = build_choices(&v, &t, d);
    assert!(c.needs_choice, "{c:?}");
    assert!(c.video.len() >= 2, "{c:?}");
    assert_eq!(c.audio.len(), 1);
    assert!(c.video[0].height > c.video[1].height);
    assert!(
        c.video
            .iter()
            .all(|x| x.available && x.requires_ffmpeg && x.id.contains('+'))
    );
    assert_eq!(c.suggested.video.as_deref(), Some(c.video[0].id.as_str()));
    // Without ffmpeg the merged choices are unavailable.
    let c2 = build_choices(&v, &Tools::default(), d);
    assert!(c2.video.iter().all(|x| !x.available));

    let out = tempfile::tempdir().unwrap();
    let mut spec = DownloadSpec::new(srv.url("ladder/manifest.mpd"), out.path());
    spec.selector = c.video.last().unwrap().id.clone();
    spec.container = Some(dman_ytdlp::Container::Mp4);
    let run = spawn(&spec, &t, "choice").unwrap();
    let Outcome::Finished { paths } = run.wait().await else {
        panic!("not finished: {:?}", files_in(out.path()))
    };
    assert_eq!(paths.len(), 1, "{:?}", files_in(out.path()));
}

#[tokio::test]
#[ignore = "needs yt-dlp and ffmpeg; set DMAN_TEST_YTDLP=1"]
async fn download_with_rate_limit_reports_progress() {
    gate!();
    let media = make_media();
    let size = std::fs::metadata(media.path().join("test.mp4"))
        .unwrap()
        .len();
    let srv = Server::start(media.path().to_path_buf()).await;
    let t = tools().await;
    let out = tempfile::tempdir().unwrap();
    let mut spec = DownloadSpec::new(srv.url("test.mp4"), out.path());
    spec.rate_limit = Some(1_000_000);
    let mut run = spawn(&spec, &t, "rl").unwrap();
    let mut events = Vec::new();
    while let Some(p) = run.next_progress().await {
        events.push(p);
    }
    let Outcome::Finished { paths } = run.wait().await else {
        panic!("not finished")
    };
    assert_eq!(paths.len(), 1);
    assert_eq!(std::fs::metadata(&paths[0]).unwrap().len(), size);
    assert!(events.len() >= 2, "{events:?}");
    assert!(events.iter().any(|p| p.total == Some(size)));
    assert_eq!(events.last().unwrap().downloaded, size);
    // Marker file is cleaned up; no .part left.
    assert!(
        files_in(out.path())
            .iter()
            .all(|f| !f.starts_with(".dman-") && !f.ends_with(".part")),
        "{:?}",
        files_in(out.path())
    );
}

#[tokio::test]
#[ignore = "needs yt-dlp and ffmpeg; set DMAN_TEST_YTDLP=1"]
async fn stop_then_resume_continues_partial_file() {
    gate!();
    let media = make_media();
    let size = std::fs::metadata(media.path().join("test.mp4"))
        .unwrap()
        .len();
    let srv = Server::start(media.path().to_path_buf()).await;
    let t = tools().await;
    let out = tempfile::tempdir().unwrap();
    let mut spec = DownloadSpec::new(srv.url("test.mp4"), out.path());
    spec.rate_limit = Some(400_000);

    let mut run = spawn(&spec, &t, "pause").unwrap();
    while let Some(p) = run.next_progress().await {
        if p.downloaded > 400_000 {
            run.stop();
            break;
        }
    }
    assert_eq!(run.wait().await, Outcome::Stopped);
    let part = files_in(out.path())
        .into_iter()
        .find(|f| f.ends_with(".part"))
        .expect(".part left behind");
    let part_len = std::fs::metadata(out.path().join(&part)).unwrap().len();
    assert!(part_len > 0 && part_len < size);

    let mut run = spawn(&spec, &t, "pause").unwrap();
    let first = tokio::time::timeout(Duration::from_secs(30), run.next_progress())
        .await
        .unwrap()
        .expect("progress after resume");
    assert!(first.downloaded > 0, "did not resume: {first:?}");
    spec.rate_limit = None;
    let Outcome::Finished { paths } = run.wait().await else {
        panic!("not finished")
    };
    assert_eq!(std::fs::metadata(&paths[0]).unwrap().len(), size);
}

#[tokio::test]
#[ignore = "needs yt-dlp and ffmpeg; set DMAN_TEST_YTDLP=1"]
async fn dash_with_ffmpeg_merges_into_one_file() {
    gate!();
    let media = make_media();
    let srv = Server::start(media.path().to_path_buf()).await;
    let t = tools().await;
    let v = probe_video(&t, &srv.url("dash/manifest.mpd")).await;
    let plan = fallback_plan(&v, QualityPreset::Best, &t);
    assert_eq!(plan.kind, PlanKind::Merged, "{v:?}");
    let out = tempfile::tempdir().unwrap();
    let mut spec = DownloadSpec::new(srv.url("dash/manifest.mpd"), out.path());
    spec.apply_plan(&plan);
    spec.container = Some(dman_ytdlp::Container::Mp4);
    let run = spawn(&spec, &t, "dash").unwrap();
    let Outcome::Finished { paths } = run.wait().await else {
        panic!("not finished: {:?}", files_in(out.path()))
    };
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].extension().unwrap(), "mp4");
    assert!(
        files_in(out.path()).iter().all(|f| !f.contains(".f")),
        "{:?}",
        files_in(out.path())
    );
}

#[tokio::test]
#[ignore = "needs yt-dlp and ffmpeg; set DMAN_TEST_YTDLP=1"]
async fn dash_without_ffmpeg_yields_separate_streams() {
    gate!();
    let media = make_media();
    let srv = Server::start(media.path().to_path_buf()).await;
    let mut t = tools().await;
    t.ffmpeg = None;
    let v = probe_video(&t, &srv.url("dash/manifest.mpd")).await;
    let plan = fallback_plan(&v, QualityPreset::Best, &t);
    assert_eq!(plan.kind, PlanKind::SeparateStreams, "{v:?}");
    let out = tempfile::tempdir().unwrap();
    let mut spec = DownloadSpec::new(srv.url("dash/manifest.mpd"), out.path());
    spec.apply_plan(&plan);
    let run = spawn(&spec, &t, "split").unwrap();
    match run.wait().await {
        Outcome::Split { files, .. } => assert!(files.len() >= 2, "{files:?}"),
        other => panic!("expected Split, got {other:?}: {:?}", files_in(out.path())),
    }
}
