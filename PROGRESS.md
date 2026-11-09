# dman — progress tracker

**Overall: 5 %** · Current milestone: **M1 — foundation** · Last updated: 2026-11-09

Resume rule: start at the first unchecked item in "Next up". Tick items here and
commit this file together with the code it describes.

## Current state

- Local repository only (no remote yet). Toolchain: Rust 1.97 stable, musl target.
- Engines available in the dev container: aria2c 1.37.0, yt-dlp 2026.10.26, ffmpeg.
- Done: proto, config, aria2 RPC client, API client. Next: aria2 supervisor/options, store, daemon, CLI.

## Milestones and weights (sum = 100 %)

| Milestone | Weight | Status |
| --- | --- | --- |
| M0 Spikes (de-risk aria2/yt-dlp behaviour, size baseline) | 5 | done |
| M1 Foundation (workspace, proto, config, aria2 client, daemon lifecycle, API, basic CLI) | 15 | in progress |
| M2 TUI core (loop, keymap, palette, downloads view, add view, confirmations) | 12 | — |
| M3 Detail and control (detail tabs, rename, limits, reorder, speed history) | 10 | — |
| M4 Daemon services (queue, disk guard, schedule, store, hooks, notifications) | 13 | — |
| M5 Search (providers, fan-out, dedupe, custom providers, search view, CLI) | 12 | — |
| M6 Video engine (yt-dlp adapter, probe UI, options, fallbacks) | 10 | — |
| M7 Web UI (htmx views, live updates, upload, auth, responsive) | 12 | — |
| M8 Remote and release (TLS, reconnect, completions, installers, update, docs, review) | 11 | — |

## Next up

### M0 — spikes (5 %)
- [x] aria2 behaviour spike: changeOption `out`/`index-out`/`bt-tracker` on paused items; magnet metadata-only → re-add with `select-file`; `bt-detach-seed-only`; session restore of paused items; upload counters after restart
- [x] yt-dlp spike: `--progress-template` JSON lines, kill + `--continue` resume, format choice without ffmpeg
- [x] Size baseline: ratatui + axum + reqwest(rustls/ring) + scraper + redb on musl, stripped

### M1 — foundation (15 %)
- [x] Workspace skeleton, lints, release profile, check script (`scripts/check.sh`) — cargo-deny config still to add in M8
- [ ] `dman-proto`: Job model, states, options, events, JSON-RPC envelope, error codes
- [x] `dman-config`: profiles, TOML load/validate, secrets.toml (0600), paths (`~/.config/dman`)
- [x] `dman-aria2`: typed JSON-RPC/WebSocket client, notifications, multicall
- [ ] `dman-aria2` supervisor + adapter: JobOptions→aria2 option mapping, capability detection (getVersion features), spawn/attach, port probe, generated aria2 config, restart with backoff, clean shutdown
- [ ] `dman-store` (redb, pulled forward from M4 because the store is authoritative): jobs, blobs (.torrent), meta counters
- [ ] `dman-daemon` skeleton: job manager (GID↔JobId), tick poller + deltas, event bus, API server (JSON-RPC over WS + POST), token auth
- [x] `dman-client`: WebSocket JSON-RPC client, hello/auth, typed methods, subscribe, ReconnectingClient (no queued actions)
- [ ] CLI (`dman` binary): `daemon start|stop|restart|status|run|gen-config`, `add`, `list`, `get`, `pause`, `resume`, `remove`, `purge`, `--json`, exit codes

### M2 … M8
Detailed task lists are written when the previous milestone finishes (see the milestone
table above and decisions below for scope).

## Confirmed decisions (do not re-ask)

| ID | Decision |
| --- | --- |
| D1 | dman daemon owns aria2c + yt-dlp + all state; TUI, web, CLI are API clients |
| D2 | TUI: ratatui + crossterm only (no GPUI in v1) |
| D3 | Web UI runs inside daemon; `dman serve` = daemon foreground with web on, or enable web on running daemon |
| D4 | API: JSON-RPC 2.0 over WebSocket + HTTP POST (one-shot) + SSE (browser) |
| D4b | Local transport: TCP localhost + token |
| D5 | dman owns the queue: add to aria2 paused, dman unpauses (priority, schedule, disk) |
| D6 | Own thin aria2 client on tokio-tungstenite |
| D7 | Web: server-rendered HTML (maud/askama) + htmx w/ SSE extension, embedded via rust-embed |
| D8 | Store: redb |
| D9 | Video pause: kill yt-dlp, resume with `--continue` |
| D10 | Video detect: known-domain heuristic + "try as video" background probe |
| D11 | Schedules: day-of-week + time windows (no cron) |
| D12 | dman persists uploaded bytes, computes ratio / seed-ratio stop itself |
| D13 | Tracker status shown as "unknown"; no scraping; per-tracker summary dropped |
| D14 | Blocklist = tracker exclusion (`bt-exclude-tracker`) |
| D15 | clap + clap_complete |
| D16 | Config dir `~/.config/dman` on Linux and macOS (etcetera, XDG strategy) |
| D17 | Clipboard: arboard read, OSC 52 copy |
| D18 | rustls + ring |
| D19 | quick-xml for Torznab/RSS/Atom |
| D20 | notify-rust; when remote, client shows notifications; daemon host optional |
| D21 | Secrets in `secrets.toml` (0600), referenced by name |
| D22 | Install: cargo-dist + Homebrew; `dman update` self-replaces only for dman-installer installs; Homebrew → `brew upgrade dman`; other/unknown → print command, refuse (exit 6) |
| D23 | Binary as small as possible without dropping features; size tracked in CI |
| D24 | Built-in TLS (user cert) + SSH/reverse-proxy docs |
| D25 | Built-in providers as declarative data where possible |
| D26 | All providers off by default; first-run prompt |
| D27 | Update check opt-in, off by default; `dman update --check` always |
| D28 | Remove-unselected-files: opt-in + confirmation |
| Q1 | Detached-only seeding = `bt-detach-seed-only` |
| Q2 | Tracker-list source refreshed on a timer, applied to new torrents |
| Q3 | Free-space threshold absolute or percent; drop mid-download → pause volume, notify, auto-resume |
| Q4 | Disconnected TUI: greyed data, actions disabled |
| Q5 | Settings writes via toml_edit, preserving comments |
| Q6 | File browser: local FS for uploads, daemon FS for directories |
| Q7 | aria2c ≥ 1.36; yt-dlp ≤ 12 months old; MSRV = stable at start (1.97) |
| Q8 | License undecided/proprietary; deps permissive only (cargo-deny) |
| Q9 | CLI output: table + `--json` |

## Key dependencies (versions checked 2026-11-09)

tokio 1.54 · serde/serde_json 1.0 · toml 1.2 / toml_edit · clap 4.6 · thiserror 2 · anyhow 1 ·
tracing 0.1 · secrecy 0.10 · etcetera 0.11 · jiff 0.2 · fs4 1.1 · nix 0.31 · tokio-util 0.7 ·
governor 0.10 · backon 1.6 · bytesize 2.7 · tokio-tungstenite 0.30 · bendy 0.6 · sha1 0.11 · url 2.5 ·
ratatui 0.30 · crossterm 0.29 · tui-input 0.15 · nucleo-matcher 0.3 · crokey 1.5 · arboard 3.7 ·
axum 0.8 · tower-http 0.7 · rust-embed 8 · maud 0.27 · subtle 2.6 · reqwest 0.13 · rustls 0.23 (ring) ·
scraper 0.29 · quick-xml 0.42 · redb 4.5 · notify-rust 4.19 · insta · assert_cmd · wiremock · proptest

## Spike findings

### aria2 1.37.0 — done (scripts were in git-ignored spikes/aria2/)
- Rename paused HTTP: `changeOption out` works. Rename paused torrent: `index-out` ("N=path", relative to `dir`, no torrent subdir); getOption shows only the last mapping → always send the FULL mapping. `select-file` works while paused.
- `bt-tracker` on paused torrent works (announceList updates, used on unpause). On ACTIVE torrent aria2 says "OK" but ignores it, and a following `pause` can fail "cannot be paused now" → dman does pause → change → resume, with retry on pause.
- aria2 does NO option validation: unknown names / garbage values return "OK". Errors always code 1, distinguished only by message ("GID … is not found", "cannot be paused now", "Cannot change option for GID#…", "No active download for GID#…"). dman validates options itself and verifies with getOption/tellStatus.
- Magnet with `bt-metadata-only=true, bt-save-metadata=true`: completes, saves `<dir>/<infohash>.torrent`, NO followedBy → dman reads the file and calls addTorrent itself (with select-file from preview).
- `bt-detach-seed-only=true` works: seeding items don't count toward max-concurrent. Seeding = status "active" + `seeder:"true"`.
- Session restore: paused/waiting items come back paused with SAME gids and options. Seeding/complete items are NOT saved unless `--force-save=true`. If `dir` doesn't exist, addTorrent silently drops the torrent from the session → dman must create `dir` before adding.
- Upload counters: without force-save a re-added torrent starts at uploadLength 0 (new gid). With force-save the `.aria2` control file keeps uploadLength across restart (but completed/removed entries are then persisted too and must be pruned).
- getVersion features here: Async DNS, BitTorrent, Firefox3 Cookie, GZip, HTTPS, Message Digest, Metalink, XML-RPC, SFTP.
- Active downloads accept changeOption for max-download-limit / max-upload-limit / seed-ratio.

### Design consequences (adopted)
- dman's store (redb) is authoritative: every job record (source, options, .torrent bytes, gid, desired state, uploaded-bytes base) is persisted by dman; aria2's session only speeds up resume of partial data. On daemon start: reconcile store ↔ aria2 (match gids; re-add missing items paused from stored source; seeding torrents re-added with stored .torrent, verify, uploaded = stored base + aria2 counter).
- Run aria2c WITHOUT force-save; persist uploaded bytes in dman every tick (D12).
- Option validation + allow-list lives in dman-aria2 (`options.rs`), verified after change.

### yt-dlp (2026.10.26) — done
- Progress: `--newline --no-colors --progress-template "download:%(progress)j"` → one JSON object per line on stdout, mixed with log lines; keep only lines starting with `{`. Keys: status, downloaded_bytes, total_bytes, tmpfilename, filename, eta, speed, elapsed (eta/speed null early). Never use `-q` (suppresses progress).
- Probe: `yt-dlp -J --no-playlist --no-warnings -- URL`; generic extractor gives a single format without vcodec/acodec/size.
- Pause (D9) works: SIGTERM and SIGKILL both leave `.part`; rerun with `--continue` resumes at the byte offset if the server supports Range; otherwise restarts from 0 and still succeeds.
- Final path: `--print-to-file after_move:filepath <file>`; but without ffmpeg a `bv*+ba` request exits 0 leaving `name.f0.mp4` + `name.f1.m4a` and after_move prints a path that does not exist.
- Exit codes: 0 ok, 1 on any failure (404, refused).
- dman must detect ffmpeg itself: use `bv*+ba/b` only with ffmpeg; without it use `b`, and treat exit 0 with `.fN.` split files as "separate streams" (offered explicitly, D-fallback), never silently.
- Download template: `yt-dlp --newline --no-colors --no-playlist --continue --part [--limit-rate N] -f SEL -o DIR/TEMPLATE --print-to-file after_move:filepath DIR/.dman-ID.final --progress-template "download:%(progress)j" -- URL`.

### Size baseline — done
- Stripped binary using ratatui, axum(ws), reqwest, rustls/ring, scraper, redb, quick-xml, clap, tokio-tungstenite:
  gnu O3 5.4 MB · musl O3 5.5 MB · musl opt-level "s" 4.4 MB (trivial usage; real app will be larger).
- Largest contributors (.text): std, redb 563 KiB, rustls 383 KiB, ring 273 KiB, clap 190 KiB, html5ever 125 KiB.
- reqwest 0.13 has no ring feature: use `default-features = false, features = ["rustls-no-provider", ...]` plus
  `rustls = { default-features = false, features = ["ring","std","tls12","logging"] }` and call
  `rustls::crypto::ring::default_provider().install_default()` at startup (else reqwest panics). aws-lc-rs absent.
- tokio-tungstenite with default-features off needs `connect` (+ `handshake`). axum 0.8.9 pulls tungstenite 0.29 —
  align our direct tokio-tungstenite version with axum's to avoid two copies (do when adding axum).
- musl builds need `musl-tools` (apt) and `CC_x86_64_unknown_linux_musl=musl-gcc` (ring's C code).

## Session log

- 2026-11-09 — plan written and all decisions confirmed; repo initialised; M0 started.
