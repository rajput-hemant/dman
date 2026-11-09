# dman — progress tracker

**Overall: 92 %** · Current milestone: **M7 — web UI leftovers, then M8** · Last updated: 2026-11-09

Resume rule: start at the first unchecked item in "Next up". Tick items here and
commit this file together with the code it describes.

## Current state

- Local repository only (no remote yet). Toolchain: Rust 1.97 stable, musl target.
- Engines available in the dev container: aria2c 1.37.0, yt-dlp 2026.10.26, ffmpeg.
- M1 done: daemon (aria2 supervisor, dman-owned queue, disk guard, reconcile after restart, redb store, WS API with token auth) + CLI (add/list/get/pause/resume/remove/purge/reorder/limit/stats/daemon …). Verified end to end with the real binary.
- Dev quick start: `cargo build -p dman`, then `DMAN_CONFIG_DIR=/tmp/x target/debug/dman daemon gen-config` (edit listen/rpc_port/download_dir), `dman daemon start`, `dman add <url>`, `dman list`.
- Real-engine tests: `DMAN_TEST_ARIA2=1 cargo test --workspace -- --include-ignored --test-threads=1`.

## Milestones and weights (sum = 100 %)

| Milestone | Weight | Status |
| --- | --- | --- |
| M0 Spikes (de-risk aria2/yt-dlp behaviour, size baseline) | 5 | done |
| M1 Foundation (workspace, proto, config, aria2 client, daemon lifecycle, API, basic CLI) | 15 | done |
| M2 TUI core (loop, keymap, palette, downloads view, add view, confirmations) | 12 | done |
| M3 Detail and control (detail tabs, rename, limits, reorder, speed history) | 10 | done |
| M4 Daemon services (queue, disk guard, schedule, store, hooks, notifications) | 13 | done |
| M5 Search (providers, fan-out, dedupe, custom providers, search view, CLI) | 12 | done |
| M6 Video engine (yt-dlp adapter, probe UI, options, fallbacks) | 10 | done |
| M7 Web UI (htmx views, live updates, upload, auth, responsive) | 12 | in progress |
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
- [x] `dman-aria2` supervisor + adapter: JobOptions→aria2 option mapping, capability detection (getVersion features), spawn/attach, port probe, generated aria2 config, restart with backoff, clean shutdown
- [x] `dman-store` (redb, pulled forward from M4 because the store is authoritative): jobs, blobs (.torrent), meta counters
- [x] `dman-daemon` skeleton: job manager (GID↔JobId), tick poller + deltas, event bus, API server (JSON-RPC over WS + POST), token auth
- [x] `dman-client`: WebSocket JSON-RPC client, hello/auth, typed methods, subscribe, ReconnectingClient (no queued actions)
- [x] CLI (`dman` binary): `daemon start|stop|restart|status|run|gen-config`, `add`, `list`, `get`, `pause`, `resume`, `remove`, `purge`, `--json`, exit codes

### M2 — TUI core (12 %)
- [x] `dman-tui` crate skeleton: Elm-style App (state/msg/update/view), terminal setup/restore (panic-safe), event loop merging crossterm input + ReconnectingClient events + ticks, render only when dirty (≤ 20 fps)
- [x] Keymap system: Action enum, per-view contexts, vim + arrow defaults, config overrides (crokey), help screen (`?`), contextual footer; test: every Action has a binding and a palette entry
- [x] Downloads view: table (name, status, progress, speed, ETA, sizes, peers/seeds, ratio, dir, source), sort keys, fuzzy filter (nucleo-matcher), multi-select, paged stopped section, global status line (speeds, counts, last result/error, disk free)
- [x] Command palette (fuzzy, every operation) + confirmation dialogs (double confirm for delete-files)
- [x] Add view: text input (tui-input, bracketed paste), multiple links, file browser for .torrent/.metalink starting at cwd, clipboard (arboard), per-add options form, preview before confirm
- [x] Disconnected state (greyed, actions disabled), engine-unavailable banner, theme + NO_COLOR, optional mouse
- [x] Wire bare `dman` to start/attach the daemon and open the TUI; insta snapshot tests at 80×24 and 120×40

### M3 — Detail and control (10 %) — pulled-forward backend
- [x] Backend: `jobs.detail` (files, peers, trackers [status unknown, D13], piece map, comment, options, per-job speed history), `stats.history`; CLI `dman get <id> --files --peers`
- [x] Detail view in TUI: tabs Files (progress, size, selection toggles while not started), Peers, Trackers (status unknown), Pieces (piece map), Speed (per-job graph), Options (editable while waiting/paused; refuse with reason when active)
- [x] Rename: waiting/paused → `out`/`index-out` (full mapping, spike); completed → move on disk + record in rename log; never for active (reason shown). API `jobs.rename`, TUI dialog, CLI `dman rename`
- [x] Magnet preview backend: `jobs.preview_magnet` (metadata-only in a private dir, not a job, cleaned up), CLI `dman preview`; new crate dman-torrent
- [x] TUI: rename dialog (Downloads + Detail) and magnet preview with file selection in the Add preview (use dman-torrent instead of the TUI's own bencode copy)
- [x] Runtime limits: per-download limits in Detail → Options; global limits/max-concurrent saved to `[limits]` via ConfigDoc (temporary override while a run window sets that field)
- [x] Global speed graph in downloads view (`w`); engine/daemon shutdown from UI is `Q` (confirmed)
- [x] Help screen lists popup keys; Esc in a non-empty Add form asks before discarding

### M4 — Daemon services (13 %)
- [x] dman-owned queue, disk guard (start refusal + pause/auto-resume), redb store, history across restarts (done in M1)
- [x] Schedule windows `[[schedule]]` (run/pause, per-window limits, DST-safe via jiff), `schedule.get`, CLI `dman schedule`
- [x] Schedule brief pushed with stats and shown in the TUI header
- [x] `dman limit` / `dman stats` show the active window and its overrides
- [x] Hooks on completion / error / end of seeding (`[notify.hooks]` commands, env DMAN_EVENT/ID/NAME/PATH/STATUS/ENGINE/SIZE, timeout, no shell unless configured), webhook POST with retry (`[notify.webhook]`), events `JobCompleted/JobFailed/SeedCompleted` (additive)
- [x] Desktop notifications (notify-rust) shown by the TUI client (D20); optional on daemon host (`[notify] desktop_on_daemon = true`)
- [x] Tracker-list source (done with M5 search integration)
- [x] Remove-unselected-files cleanup (D28: opt-in + confirmation) after torrent completion
- [x] Real-aria2 tests for seeding/ratio stop (dman now enforces seed ratio/time itself; fixed re-add of completed torrents with check-integrity) (D12) using the local seeder fixture (crates/dman-daemon/tests/fixtures/seeder.py)
- [x] `config.reload` (limits, schedule, torrents, notify, search, video live; aria2/web/profile need restart)
- [ ] Follow-up: store .torrent metadata for magnet jobs on follow-up so seeding survives restarts; web UI for cleanup/reload

### M5 — Search (12 %)
- [x] `dman-search` library: declarative provider defs (torznab/feed/html/json), built-ins (Nyaa, YTS, TPB, Knaben, 1337x, Torznab template; all off, unverified against live sites), concurrent fan-out with per-provider timeout/rate limit/cap, dedupe by info-hash, ranking, test_provider, import/export, tracker-list fetch
- [x] Daemon: providers config (`[[provider]]` + built-in overrides by name), `search.start/cancel/results/history`, results streamed as events, search history persisted (redb), `providers.list/set/test/import/export`, secrets for API keys, runtime changes without restart
- [x] TUI Search view: provider selector, query, live results table (title, size, S/L, source, date), filters (size, seeders, source), add from results (magnet or detail resolve), history + re-run, per-provider errors; first-run prompt to enable providers (D26)
- [x] Settings view basics for providers (enable/disable/reorder/test/import/export)
- [x] CLI `dman search <q> [--provider X] [--add N]`, `dman search --history`, `dman providers ...`, `dman trackers status`; tracker-list source (Q2) fetched/cached/attached in daemon
- [x] Live-verified Nyaa, TPB (fixed HTML entities), Knaben; 1337x (403 Cloudflare) and YTS (unreachable) stay unverified

### M6 — Video engine (10 %)
- [x] `dman-ytdlp` library: tool detection (+age warning), URL heuristic (D10), probe (video/playlist), quality presets → selectors honouring ffmpeg, fallback plans (merged / single / separate streams), arg builder (--ignore-config, allow-listed extra args), spawn with JSON progress, stop/resume via --continue, outcome classification (Finished/Split/Failed/Stopped)
- [x] Daemon: Engine::YtDlp jobs in the same queue/history/hooks/notifications; `video.probe` API; `jobs.add` with a `Video` source (spec: format, quality, container, playlist items, subs, thumbnail, metadata, template, cookies, rate limit, sponsorblock); pause = stop, resume = respawn; progress → Job figures; config `[video]` (paths, default quality, template, sponsorblock defaults)
- [x] Global download limit also throttles yt-dlp (equal-share budget, respawn with hysteresis)
- [x] TUI: Add view detects video URLs (heuristic + "try as video"), probe overlay with format/quality/container/playlist-item selection and fallback explanation; Detail view for video jobs
- [x] CLI: `dman add --video [--quality 1080p --container mkv --subs en --playlist-items 1-3 ...]`, `dman probe <url>`
- [x] Missing yt-dlp/ffmpeg: explained via engine.status (yt-dlp entry, ffmpeg as feature) (engine.status entry for yt-dlp and ffmpeg)
- [x] Follow-up: use `nix::sys::signal::killpg` instead of the external `kill` binary in dman-ytdlp

### M7 — Web UI (12 %)
- [x] `dman-web` crate: `Backend` trait + `router()`; htmx 2.0.4 + SSE ext (0BSD, 65 KB assets), pages for downloads/detail/add/search/settings, SSE live rows (≤ 2/s), token login + bearer, CSRF (Origin/Sec-Fetch-Site + per-session token), CSP without inline code, base_path, upload ≤ 10 MiB, double confirmation for delete-files
- [x] Daemon wiring: implement `Backend` over the dispatch, `[web]` config (enabled, bind, port, base_path, token_ref, exposed warning), mount on its own listener, `dman serve` (D3: foreground daemon with web on / enable on running daemon), `web.status` API
- [x] Use engine.status (yt-dlp availability) instead of the empty-URL video.probe hack for showing video options
- [x] Browser smoke test with Playwright (49/49 checks, desktop + iPhone 13; fixed: per-add download limit, no-provider search message, logout styling)
- [ ] Web UX: phone filter/sort controls too tall; refused delete-confirm should be inline; detail speed row via SSE instead of 3 s polling
- [ ] Follow-ups found by the browser test: resume of an HTTP download from a server without Range support ends in aria2 "No URI available" → restart from zero automatically or explain; `dman ... | head` panics on SIGPIPE (println!) — handle broken pipe in the CLI; decide whether removed jobs should stay listed (currently state `removed` until purged) (desktop + phone viewport): load, add a download, live progress, pause/resume, remove with delete confirmation, search
- [ ] Missing web features vs TUI: magnet file selection (preview_magnet), daemon folder picker (fs.list), multi-select

### M8 — Remote and release (11 %)
- [x] Built-in TLS (D24) for the API listener and web listener (rustls, user cert/key paths in config; `wss://`/`https://` in client + remote profiles) + docs for SSH tunnel / reverse proxy
- [x] Remote profiles end to end (TLS + custom CA pin, reconnect across daemon restart, docs/remote.md): TUI/CLI against a remote daemon (paths are daemon-side; uploads for local .torrent), reconnect hardening tests (kill/restart daemon while TUI attached)
- [ ] CLI robustness: SIGPIPE-safe output, consistent exit codes audit
- [x] Packaging: cargo-dist config (Linux musl x86_64/aarch64, macOS universal), Homebrew tap formula, install receipt; `dman update` with install-method detection (D22) + `--check`; opt-in update check (D27)
- [x] cargo-deny (licenses permissive only + MPL-2.0 exception — Q8, advisories), CI workflow (GitHub Actions: fmt, clippy, test, gated aria2/yt-dlp jobs, size report)
- [ ] README (install, quick start, config reference, security notes), `dman --help` polish, man page optional
- [ ] Security review: secrets never logged (grep logs in tests), path traversal (rename/delete/cleanup), webhook SSRF note, web auth, CSP; fix findings
- [ ] Final size pass: measure, consider optional features (notify-rust/web/search/video as cargo features, default all on)

### Follow-ups noted during M1
- [x] Split `crates/dman-daemon/src/manager.rs` into `manager/` modules (largest 524 lines).
- [x] `AddOutcome::Added` now has `extra_ids` (metalinks with several files).
- [x] `limits.set` persisted to config (done with M4 schedule work).
- [x] `engine.restart` method + background retry every 60 s while the engine is unavailable; CLI `dman daemon engine-restart`.
- Magnet follow-up, seeding, metalink and adoption paths are not yet tested against real aria2 (needs local tracker + seeder fixture from spikes/aria2/lib.py idea) — add in M4.
- cargo-deny config (M8).

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
| Q8 | License undecided/proprietary; deps permissive only (cargo-deny) — exception: MPL-2.0 allowed (file-level copyleft; scraper deps, nucleo-matcher), approved 2026-11-09 |
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

- 2026-11-09 — plan written and all decisions confirmed; repo initialised; M0 done; M1 done (135 tests); M2 done (278 tests); M3 done; M4 schedules (372 tests); hooks/webhook/notifications; dman-search library (52 tests). Release musl binary 10.5 MB (notify-rust/zbus + reqwest added ~5 MB).
