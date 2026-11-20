# dman

[![CI](https://github.com/rajput-hemant/dman/actions/workflows/ci.yml/badge.svg)](https://github.com/rajput-hemant/dman/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

A terminal-first download manager for torrents, magnet links, Metalink, HTTP/FTP/SFTP and
video sites, with a full-screen TUI, a web UI and a scriptable CLI in one binary.

dman drives [aria2](https://aria2.github.io/) for transfers and, optionally,
[yt-dlp](https://github.com/yt-dlp/yt-dlp) for video. A background daemon owns the queue, so
downloads keep running after you close the interface. The TUI, web UI and CLI are clients of
one JSON-RPC-over-WebSocket API, so any of them can also control a daemon on another machine.

Full documentation: <https://rajput-hemant.github.io/dman/>

## Features

- Magnets (with file selection before start), torrents, Metalink, HTTP/FTP/SFTP, video sites and public Google Drive folders
- Queue with concurrency limits, speed limits and schedule windows
- Built-in torrent search over configurable providers (off until you enable them)
- Video quality picker with a saved default
- Hooks, webhooks and desktop notifications
- Browser integration for magnet links and `.torrent` files
- Works against a remote daemon over TLS

## Requirements

- Linux, macOS or Windows.
- `aria2c` 1.36+ (required). Without it the daemon still starts and the interfaces work, but
  nothing downloads and the engine shows as unavailable; install it, then run
  `dman daemon engine-restart`.
- `yt-dlp` (optional) for video sites, and `ffmpeg` (optional) for merging video and audio.
  Without them dman downloads everything else as usual.

## Install

Homebrew, Scoop, winget and the AUR package install aria2 together with dman. With the other
methods, install `aria2c` yourself.

| Method | Platform | Command |
| --- | --- | --- |
| Shell installer | Linux, macOS | `curl --proto '=https' --tlsv1.2 -LsSf https://github.com/rajput-hemant/dman/releases/latest/download/dman-installer.sh \| sh` |
| PowerShell installer | Windows | `powershell -ExecutionPolicy Bypass -c "irm https://github.com/rajput-hemant/dman/releases/latest/download/dman-installer.ps1 \| iex"` |
| Homebrew | macOS, Linux | `brew install rajput-hemant/tap/dman` |
| Scoop | Windows | `scoop bucket add rajput-hemant https://github.com/rajput-hemant/scoop-bucket` then `scoop install rajput-hemant/dman` |
| winget | Windows | `winget install rajput-hemant.dman` |
| AUR | Arch Linux | `yay -S dman-bin` |
| crates.io | any (builds from source) | `cargo install --locked dman` |

Prebuilt archives, updating and building from source are covered in
[Install](website/guide/install.md).

## Quick start

```bash
dman                                       # start the daemon if needed and open the TUI
dman add "magnet:?xt=urn:btih:..."         # add from the command line
dman add https://example.org/f.iso --wait  # show progress, exit when done
dman list --sort speed                     # show downloads (--json for scripts)
dman providers enable nyaa                 # search providers start disabled
dman search ubuntu                         # search, then pick results to add
dman serve                                 # web UI at http://127.0.0.1:6811/
dman completions install                   # tab completion for your shell
dman --help                                # everything else
```

## Documentation

| Page | Covers |
| --- | --- |
| [Getting started](website/guide/getting-started.md) | Requirements, quick start, how the pieces fit |
| [Install](website/guide/install.md) | Every install channel, prebuilt archives, updating, building from source |
| [Configuration](website/guide/configuration.md) | `config.toml`, profiles, secrets, limits, schedule, torrents, notifications |
| [CLI](website/guide/cli.md) | Commands, JSON output, exit codes, Google Drive folders |
| [TUI](website/guide/tui.md) | Keys and the full-screen interface |
| [Web UI](website/guide/web-ui.md) | `dman serve`, access model, `[web]` settings |
| [Remote use](website/guide/remote.md) | A daemon on another machine: tokens, SSH tunnel, TLS, reverse proxy |
| [Search](website/guide/search.md) | Providers, mirrors, proxies, DNS over HTTPS, custom providers |
| [Browser](website/guide/browser.md) | Magnet and `.torrent` handler, bookmarklet, userscript, clipboard |
| [Troubleshooting](website/guide/troubleshooting.md) | Common failures and fixes |

Maintainers: [docs/releasing.md](docs/releasing.md) covers the release process.

## Development

```bash
cargo build -p dman
./scripts/check.sh      # fmt, clippy and tests, with and without default features
```

Tests against the real engines: `DMAN_TEST_ARIA2=1 DMAN_TEST_YTDLP=1 cargo test --workspace -- --test-threads=1`.
The website in `website/` is a VitePress site (`bun install && bun run dev` there).

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
