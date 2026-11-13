# dman

[![CI](https://github.com/rajput-hemant/dman/actions/workflows/ci.yml/badge.svg)](https://github.com/rajput-hemant/dman/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

A terminal-first download manager for torrents, magnet links, Metalink, HTTP/FTP/SFTP and
video sites, with a full-screen TUI, a web UI and a scriptable CLI in one binary.

## TL;DR

- Install `aria2c` and dman (see [Install](#install)), then run `dman`. The daemon starts on demand and the TUI opens.
- `dman add URL` adds a download from the command line; `dman serve` turns on the web UI at `http://127.0.0.1:6811/`.
- dman drives [aria2](https://aria2.github.io/) for transfers and, optionally, [yt-dlp](https://github.com/yt-dlp/yt-dlp) for video. A background daemon owns the queue, so downloads keep running after you close the interface.
- The TUI, web UI and CLI are clients of one JSON-RPC-over-WebSocket API, so any of them can also control a remote daemon.

## Contents

- [Features](#features)
- [Requirements](#requirements)
- [Install](#install)
- [Quick start](#quick-start)
- [Usage](#usage)
- [Configuration](#configuration)
- [Documentation](#documentation)
- [Building from source](#building-from-source)
- [Development](#development)
- [License](#license)

## Features

- Magnets (with file selection before start), torrents, Metalink, HTTP/FTP/SFTP and video sites
- Queue with concurrency limits, speed limits and schedule windows
- Built-in torrent search over configurable providers (off until you enable them)
- Video quality picker with a saved default
- Hooks, webhooks and desktop notifications
- Works against a remote daemon over TLS

## Requirements

| Tool | Status | Notes |
| --- | --- | --- |
| Linux, macOS or Windows | required | See [Install](#install) for the supported builds |
| `aria2c` 1.36+ | required | Without it the daemon still starts and the interfaces work, but nothing downloads and the engine shows as unavailable. Install it, then run `dman daemon engine-restart`. |
| `yt-dlp` | optional | Video downloads. Without it, video downloads are disabled; other URLs download as usual and `--no-video` forces a plain download of a page or file. |
| `ffmpeg` | optional | Merging separate video and audio, and embedding subtitles, thumbnails and metadata. Without it, dman picks the best single-file quality instead (`dman probe URL` shows what is possible). |
| Rust (stable, 1.97+) | source builds only | See [Building from source](#building-from-source) |

## Install

Homebrew, Scoop, winget and the AUR package install aria2 together with dman. With the other
methods, install `aria2c` yourself; `yt-dlp` and `ffmpeg` are optional everywhere.

| Method | Platform | Command |
| --- | --- | --- |
| Shell installer | Linux, macOS | `curl --proto '=https' --tlsv1.2 -LsSf https://github.com/rajput-hemant/dman/releases/latest/download/dman-installer.sh \| sh` |
| PowerShell installer | Windows | `powershell -ExecutionPolicy Bypass -c "irm https://github.com/rajput-hemant/dman/releases/latest/download/dman-installer.ps1 \| iex"` |
| Homebrew | macOS, Linux | `brew install rajput-hemant/tap/dman` |
| Scoop | Windows | `scoop bucket add rajput-hemant https://github.com/rajput-hemant/scoop-bucket` then `scoop install rajput-hemant/dman` |
| winget | Windows | `winget install rajput-hemant.dman` |
| AUR | Arch Linux | `yay -S dman-bin` |
| crates.io | any (builds from source) | `cargo install --locked dman` |

Prebuilt archives on the [releases page](https://github.com/rajput-hemant/dman/releases):

| Platform | Target |
| --- | --- |
| Linux x86_64 | `x86_64-unknown-linux-musl` |
| Linux aarch64 | `aarch64-unknown-linux-musl` |
| macOS Intel | `x86_64-apple-darwin` |
| macOS Apple silicon | `aarch64-apple-darwin` |
| Windows x86_64 | `x86_64-pc-windows-msvc` |

Installs made with an installer update themselves with `dman update`; for the other methods it
prints the package manager's command. Maintainers: see [docs/releasing.md](docs/releasing.md).

Then enable shell completions:

```bash
dman completions install
```

### Build features

Slimmer source builds are possible with `--no-default-features --features web,search`.

| Feature | Provides |
| --- | --- |
| `web` | `dman serve` and the browser UI |
| `search` | Torrent search, providers and the public tracker list |
| `video` | The yt-dlp video engine |
| `notify` | Desktop notifications (daemon and TUI) |

All four are on by default.

## Quick start

```bash
dman                                       # start the daemon if needed and open the TUI
dman add "magnet:?xt=urn:btih:..."         # add from the command line
dman add https://example.org/f.iso --wait  # show progress, exit when done
dman list --sort speed                     # show downloads (--json for scripts)
dman providers enable nyaa                 # search providers start disabled
dman search ubuntu                         # search, then pick results to add
dman serve                                 # web UI at http://127.0.0.1:6811/
dman --help                                # everything else
```

## Usage

### TUI

Run `dman` with no arguments.

| Key | Action |
| --- | --- |
| `a` | Add a download |
| `f` | Search |
| `Enter` | Details |
| `p` / `r` | Pause / resume |
| `d` | Remove |
| `s` | Sort |
| `/` | Filter |
| `:` | Command palette |
| `?` | All keys |
| `q` | Quit (downloads keep running) |

### Web UI

`dman serve` turns the web UI on in the running daemon; `--persist` keeps it on.

| Setting | Behavior |
| --- | --- |
| Default address | `127.0.0.1:6811`, no login |
| Network address (`--bind 0.0.0.0`) | Requires an access token; `--print-token` shows it |

See [docs/remote.md](docs/remote.md) for tokens, TLS and reverse proxies.

### CLI

Every command is scriptable: the global `--json` flag gives machine-readable output (also for
errors) and `--profile NAME` (or `DMAN_PROFILE`) selects a daemon profile. Run `dman --help` for
the full list and `dman <command> --help` for flags and examples.

| Task | Commands |
| --- | --- |
| Add and wait | `dman add URL`, `dman add --clipboard`, `dman add --wait` |
| Inspect | `dman list`, `dman get ID`, `dman stats`, `dman probe URL` |
| Control | `dman pause`, `dman resume`, `dman remove`, `dman limit`, `dman max-concurrent` |
| Search | `dman search`, `dman providers`, `dman trackers status` |
| Daemon | `dman daemon start\|stop\|restart\|status\|engine-restart\|gen-config` |
| Web UI | `dman serve`, `dman serve --stop` |
| Maintenance | `dman config`, `dman update`, `dman completions install` |

### Remote use

Point a profile at another machine's daemon (`kind = "remote"`) and use the TUI, CLI and web UI
against it, over an SSH tunnel, built-in TLS or a reverse proxy. See
[docs/remote.md](docs/remote.md).

### Browser integration

`dman integrate install` makes dman the handler of magnet links and `.torrent` files (Linux and
Windows); a bookmarklet and userscript can send any link to the web UI. See
[docs/browser.md](docs/browser.md).

### Search providers

Built-in providers (The Pirate Bay, Nyaa, Knaben, YTS, EZTV, Internet Archive and more) are all
off until enabled:

```bash
dman providers list
dman providers enable NAME
dman providers test NAME
```

Providers marked unverified were written from the public format and may not work; Torznab
(Prowlarr/Jackett) covers Cloudflare-protected sites such as 1337x. See
[docs/search.md](docs/search.md) for the list, mirrors, proxies, DNS over HTTPS and custom
providers.

## Configuration

The config file is `~/.config/dman/config.toml` (`dman config path` prints the location;
`DMAN_CONFIG_DIR` relocates everything). `dman daemon gen-config` writes it; defaults apply
without a file.

| Command | Purpose |
| --- | --- |
| `dman config show` | Effective settings, secrets redacted |
| `dman config get limits` | One setting or a section |
| `dman config set limits.download 5MiB` | Change a setting; comments and layout are kept |
| `dman config edit` | Edit in `$EDITOR`; checked before saving |
| `dman config reload` | Apply to the running daemon |

Torrents stop seeding when they finish unless `[torrents] seed = true`.

## Documentation

| Page | Covers |
| --- | --- |
| [docs/remote.md](docs/remote.md) | Using a daemon on another machine: tokens, SSH tunnel, TLS, reverse proxy |
| [docs/search.md](docs/search.md) | Search providers, mirrors, proxies, DNS over HTTPS, custom providers |
| [docs/browser.md](docs/browser.md) | Sending magnet links, `.torrent` files and URLs from the browser |
| [docs/releasing.md](docs/releasing.md) | Release process and package-manager publishing |

## Building from source

The toolchain is `channel = "stable"` (`rust-toolchain.toml`); the minimum supported Rust
version is 1.97 (`rust-version` in `Cargo.toml`). `rustup` installs the toolchain on first use.

```bash
cargo build --release -p dman
```

Cross-compiling:

| Target | Command | Needs |
| --- | --- | --- |
| Linux musl or macOS | `cargo zigbuild --release -p dman --target <linux-musl or apple target>` | zig, cargo-zigbuild |
| Windows | `cargo xwin build --release -p dman --target x86_64-pc-windows-msvc` | cargo-xwin, llvm |

All five release targets and their setup are listed in
[Building for all platforms locally](docs/releasing.md#building-for-all-platforms-locally).
Official releases are built by CI with cargo-dist; see [docs/releasing.md](docs/releasing.md).

## Development

```bash
cargo build -p dman
./scripts/check.sh      # fmt, clippy and tests, with and without default features
```

Tests against the real engines: `DMAN_TEST_ARIA2=1 DMAN_TEST_YTDLP=1 cargo test --workspace -- --test-threads=1`.

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
