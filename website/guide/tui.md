# TUI

Run `dman` with no arguments. It starts the daemon if needed and opens the full-screen
interface.

```bash
dman
```

Downloads keep running when you quit: the background daemon owns the queue, the TUI is only a
client of it.

## Keys

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

## Search from the TUI

`f` opens the same search as `dman search`: results come from the enabled
[providers](./search) and you pick which to add. Providers are off until you
enable one, for example `dman providers enable nyaa`.

## Remote daemon

A TUI can point at a daemon on another machine: `--profile work` (or `DMAN_PROFILE=work`)
selects a remote profile. See [Remote use](./remote).

## Disconnects

The TUI reconnects automatically with backoff after a network drop or daemon restart. Actions
fail immediately while disconnected (nothing is queued); it re-syncs once connected.
