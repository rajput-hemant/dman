# dman

A terminal-first download manager for torrents, magnet links, Metalink, HTTP/FTP/SFTP
and video sites — one binary with a full-screen TUI, a web UI and a scriptable CLI.

dman drives [aria2](https://aria2.github.io/) for the transfers and, optionally,
[yt-dlp](https://github.com/yt-dlp/yt-dlp) for video. A small background daemon owns the
queue, so downloads keep running after you close the interface.

## Features

- Torrents, magnets (with file selection before start), Metalink, HTTP/FTP/SFTP, video sites
- Queue with concurrency limits, speed limits and schedule windows
- Built-in torrent search across configurable providers (Torznab/Prowlarr/Jackett, RSS, JSON, HTML)
- Video quality picker with a saved default
- Hooks, webhooks and desktop notifications
- Works against a remote daemon over TLS

## Requirements

- `aria2c` 1.36+
- Optional: `yt-dlp` and `ffmpeg` for video downloads
- To build from source: Rust (the pinned toolchain installs automatically via `rustup`)

## Install

From source:

```bash
cargo install --locked --git https://github.com/rajput-hemant/dman dman
```

Then enable shell completions:

```bash
dman completions install
```

## Quick start

```bash
dman                                   # start the daemon if needed and open the TUI
dman add "magnet:?xt=urn:btih:..."     # add from the command line
dman list                              # show downloads (add --json for scripts)
dman search ubuntu --add 1             # search providers and add the first result
dman serve                             # web UI at http://127.0.0.1:6811/
```

In the TUI press `a` to add, `f` to search, `Enter` for details, `:` for the command
palette and `?` for every key.

## Configuration

Settings live in `~/.config/dman/config.toml` (created by `dman daemon gen-config`):
download folder, limits, schedule, search providers, video defaults, notifications,
web UI and remote profiles. Most changes apply with `dman config reload`.

Torrents stop seeding when they finish unless `[torrents] seed = true` is set.
Search providers are all off until you enable them (`dman providers enable <name>`).

## Remote use

Point a profile at another machine's daemon (`kind = "remote"`) to use the TUI, CLI and
web UI against it. See [docs/remote.md](docs/remote.md) for TLS, tokens and SSH tunnels.

## Development

```bash
cargo build -p dman
cargo test --workspace
```

Real-engine tests run with `DMAN_TEST_ARIA2=1 DMAN_TEST_YTDLP=1 cargo test --workspace -- --test-threads=1`.
