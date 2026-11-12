# dman

A terminal-first download manager for torrents, magnet links, Metalink, HTTP/FTP/SFTP and
video sites: one binary with a full-screen TUI, a web UI and a scriptable CLI.

dman drives [aria2](https://aria2.github.io/) for the transfers and, optionally,
[yt-dlp](https://github.com/yt-dlp/yt-dlp) for video. A small background daemon owns the
queue, so downloads keep running after you close the interface. The daemon starts on demand.

- Magnets (with file selection before start), torrents, Metalink, HTTP/FTP/SFTP, video sites
- Queue with concurrency limits, speed limits and schedule windows
- Built-in torrent search over configurable providers (off until you enable them)
- Video quality picker with a saved default
- Hooks, webhooks and desktop notifications
- Works against a remote daemon over TLS

## Requirements

- Linux, macOS or Windows
- `aria2c` 1.36+ (required). Without it the daemon still starts and the interfaces work, but
  nothing downloads and the engine shows as unavailable; install it and run
  `dman daemon engine-restart`.
- `yt-dlp` (optional): video downloads. Without it, video downloads are disabled; other
  URLs download as usual and `--no-video` forces a plain download of a page or file.
- `ffmpeg` (optional): merging separate video and audio, and embedding subtitles, thumbnails
  and metadata. Without it, dman picks the best single-file quality instead
  (`dman probe URL` shows what is possible).
- To build from source: Rust (the pinned toolchain installs via `rustup`)

## Install

From source:

```bash
cargo install --locked --git https://github.com/rajput-hemant/dman dman
```

Slimmer builds: `--no-default-features --features web,search` (features: `web`, `search`, `video`, `notify`; all on by default).

Then enable shell completions:

```bash
dman completions install
```

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

TUI keys: `a` add, `f` search, `Enter` details, `p`/`r` pause/resume, `d` remove, `s` sort,
`/` filter, `:` command palette, `?` all keys, `q` quit (downloads keep running).

## Web UI

`dman serve` turns the web UI on in the running daemon (`--persist` keeps it on). It listens on
`127.0.0.1:6811` without a login; binding to a network address (`--bind 0.0.0.0`) requires an
access token (`--print-token`). See [docs/remote.md](docs/remote.md).

## Configuration

`~/.config/dman/config.toml` (`dman config path`; `DMAN_CONFIG_DIR` relocates everything) is
written by `dman daemon gen-config` (defaults apply without a file).

```bash
dman config show                       # effective settings, secrets redacted
dman config get limits                 # one setting or a section
dman config set limits.download 5MiB   # comments and layout are kept
dman config edit                       # edit in $EDITOR; checked before saving
dman config reload                     # apply to the running daemon
```

Torrents stop seeding when they finish unless `[torrents] seed = true`.

## Search providers

Built-in providers (The Pirate Bay, Nyaa, Knaben, YTS, EZTV, Internet Archive and more) are all
off until enabled: `dman providers list`, `dman providers enable NAME`,
`dman providers test NAME`. Providers marked unverified were written from the public format
and may not work; Torznab (Prowlarr/Jackett) covers Cloudflare-protected sites such as 1337x.
See [docs/search.md](docs/search.md) for the list, mirrors, proxies, DNS over HTTPS and custom
providers.

## Remote use

Point a profile at another machine's daemon (`kind = "remote"`) and use the TUI, CLI and web
UI against it, over an SSH tunnel, built-in TLS or a reverse proxy. See
[docs/remote.md](docs/remote.md).

## Browser integration

`dman integrate install` makes dman the handler of magnet links and `.torrent` files (Linux and
Windows); a bookmarklet and userscript can send any link to the web UI. See
[docs/browser.md](docs/browser.md).

## Development

```bash
cargo build -p dman
./scripts/check.sh      # fmt, clippy and tests, with and without default features
```

Tests against the real engines: `DMAN_TEST_ARIA2=1 DMAN_TEST_YTDLP=1 cargo test --workspace -- --test-threads=1`.
