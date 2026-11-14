---
name: dman
description: 'Drives dman, a terminal-first download manager: installs it, adds and controls downloads, and picks between its TUI, CLI and web UI.'
---

# dman

dman is a terminal-first download manager for torrents, magnets, Metalink, HTTP/FTP/SFTP and video sites, with a TUI, a web UI and a scriptable CLI in one binary. It needs `aria2c` 1.36+ for transfers; `yt-dlp` (video) and `ffmpeg` (merging) are optional.

## Install

| Platform | Command |
| --- | --- |
| macOS, Linux (shell) | `curl --proto '=https' --tlsv1.2 -LsSf https://github.com/rajput-hemant/dman/releases/latest/download/dman-installer.sh \| sh` |
| Windows (PowerShell) | `powershell -ExecutionPolicy Bypass -c "irm https://github.com/rajput-hemant/dman/releases/latest/download/dman-installer.ps1 \| iex"` |
| macOS, Linux (brew) | `brew install rajput-hemant/tap/dman` |
| Windows (scoop) | `scoop bucket add rajput-hemant https://github.com/rajput-hemant/scoop-bucket && scoop install rajput-hemant/dman` |
| Windows (winget) | `winget install rajput-hemant.dman` |
| Any (source) | `cargo install --locked dman` |

Homebrew, Scoop, winget and the AUR ship aria2 with dman; the other methods need `aria2c` installed separately. Finish with `dman completions install`.

## Daemon model

The CLI, TUI and web UI are clients of one background daemon over JSON-RPC, so downloads keep running after any interface closes. Manage it with `dman daemon start|stop|restart|status|engine-restart|gen-config`. The web UI defaults to `http://127.0.0.1:6811/`.

## Commands

| Command | Effect |
| --- | --- |
| `dman add URL` | add a download: URL, magnet, `.torrent`/`.metalink`/`.meta4` file, or a video URL (`--wait`, `--clipboard`, `--paused`, `--dir DIR`, `--force`) |
| `dman list` | list downloads (`--sort speed`, `--state active,paused`, `--limit N`, `--json`, `--watch`) |
| `dman pause ID` or `dman pause --all` | pause downloads |
| `dman resume ID` or `dman resume --all` | resume downloads |
| `dman remove ID` | remove a download (asks; `--yes` for scripts, `--delete-files` deletes files too) |
| `dman get ID` | one download's details (`--files`, `--peers`, `--watch`) |
| `dman serve` | web UI on `http://127.0.0.1:6811/` (`--persist` keeps it on, `--stop` turns it off) |
| `dman daemon start` | start the background daemon (also stop, restart, status, engine-restart, gen-config) |

## Which interface

- TUI: interactive browsing, sorting, filtering, file selection - run `dman` with no arguments; `?` lists all keys.
- CLI: scripts and agents - every command takes the global `--json`; destructive commands need `--yes` when stdin is not a terminal.
- Web UI: control from a browser or another machine - `dman serve`.

## Examples

```bash
dman add "magnet:?xt=urn:btih:..." --select-files 1,3-5
dman add https://example.org/big.iso --wait && ./next-step.sh
dman list --json | jq '.[].name'
```
