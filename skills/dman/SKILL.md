---
name: dman
description: 'Drives dman, a terminal-first download manager: installs it, adds and controls downloads, and picks between its TUI, CLI and web UI.'
---

# dman

dman is a terminal-first download manager for torrents, magnets, Metalink, HTTP/FTP/SFTP and video sites, with a TUI, a web UI and a scriptable CLI in one binary. It needs `aria2c` 1.36+ for transfers; `yt-dlp` (video) and `ffmpeg` (merging) are optional.

## Install

Pick a channel from the [install guide](https://rajput-hemant.github.io/dman/guide/install) (shell or PowerShell installer, Homebrew, Scoop, winget, AUR, `cargo install --locked dman`). Homebrew, Scoop, winget and the AUR ship aria2 with dman; the other methods need `aria2c` installed separately. Finish with `dman completions install`.

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
| `dman wait ID` | block until downloads finish (exit 0 all completed, 8 some failed, 124 on `--timeout`) |
| `dman set ID` | change a download's options (`--max-download-limit`, `--select`, `--dir`, `--position`, ...) |
| `dman search WORDS` | search enabled providers (`dman providers enable NAME` first; `--add 1,2` adds results) |
| `dman serve` | web UI on `http://127.0.0.1:6811/` (`--persist` keeps it on, `--stop` turns it off) |
| `dman daemon start` | start the background daemon (also stop, restart, status, engine-restart, gen-config) |

## Which interface

- TUI: interactive browsing, sorting, filtering, file selection - run `dman` with no arguments; `?` lists all keys.
- CLI: scripts and agents - every command takes the global `--json`; destructive commands need `--yes` when stdin is not a terminal.
- Web UI: control from a browser or another machine - `dman serve`.

Add also takes a public Google Drive folder link, which becomes one download per file. Exit codes (9 = already in the list, use `--force`; 7 = engine missing) and every other command are in the [CLI guide](https://rajput-hemant.github.io/dman/guide/cli).

## Examples

```bash
dman add "magnet:?xt=urn:btih:..." --select-files 1,3-5
dman add https://example.org/big.iso --wait && ./next-step.sh
dman list --json | jq '.[].name'
```
