# Getting started

dman drives [aria2](https://aria2.github.io/) for transfers and, optionally,
[yt-dlp](https://github.com/yt-dlp/yt-dlp) for video. A background daemon owns the queue, so
downloads keep running after you close the interface. The TUI, web UI and CLI are clients of
one JSON-RPC-over-WebSocket API.

## Requirements

| Tool | Status | Notes |
| --- | --- | --- |
| Linux, macOS or Windows | required | See [Install](./install) for the supported builds |
| `aria2c` 1.36+ | required | Without it nothing downloads and the engine shows as unavailable. Install it, then run `dman daemon engine-restart`. |
| `yt-dlp` | optional | Video downloads. Without it video is disabled; other URLs download as usual. |
| `ffmpeg` | optional | Merging separate video and audio, and embedding subtitles, thumbnails and metadata. Without it dman picks the best single-file quality. |

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

## How it fits together

- `dman` with no arguments starts the daemon if needed and opens the
  [TUI](./tui).
- `dman add URL` adds a download from the command line; see the
  [CLI](./cli) page for the rest.
- `dman serve` turns on the [web UI](./web-ui) at `http://127.0.0.1:6811/`.
- Settings live in `~/.config/dman/config.toml`; see
  [Configuration](./configuration).
- To use a daemon on another machine, see [Remote use](./remote).

Shell completions for IDs, providers, profiles and options are installed with:

```bash
dman completions install
```
