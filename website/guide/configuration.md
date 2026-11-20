# Configuration

The config file is `~/.config/dman/config.toml` (`dman config path` prints the location;
`DMAN_CONFIG_DIR` relocates everything). `dman daemon gen-config` writes a commented file with
every setting and its default, unless one exists; defaults apply without a file.

## Reading and changing settings

| Command | Purpose |
| --- | --- |
| `dman config show` | Effective settings, secrets redacted |
| `dman config path` | Where the config file is |
| `dman config get limits` | One setting or a section |
| `dman config set limits.download 5MiB` | Change a setting; comments and layout are kept |
| `dman config edit` | Edit in `$EDITOR`; checked before saving |
| `dman config unset KEY` | Remove a setting, so the default applies again |
| `dman config reload` | Apply to the running daemon |

`reload` applies what can change live: limits, schedule, notifications, search, torrents and
video defaults. Anything else, such as a listener address, needs `dman daemon restart`.

## Sections

| Section | Controls | Default |
| --- | --- | --- |
| `default_profile`, `[profiles.NAME]` | Which daemon clients talk to, download directory, free-space guard | `local`, `~/Downloads`, pause below 1GiB free |
| `[limits]` | Global speed limits and `max_concurrent` | unlimited, 3 |
| `[[schedule]]` | Time windows that run with other limits or pause | none |
| `[torrents]` | Seeding and unselected files | seeding off |
| `[notify]` | Desktop notifications, webhook, hooks | desktop on |
| `[search]` | Providers, proxy, DNS, tracker list | all providers off; see [Search](./search) |
| `[video]` | yt-dlp and ffmpeg paths, quality picker, subtitles | picker on, quality `best` |
| `[web]` | The browser UI | off; see [Web UI](./web-ui) |
| `[ui]` | TUI theme, colors, icons, mouse | `dman-dark`, mouse off |
| `[aria2]` | Path and RPC port of the engine | `aria2c`, 6800 |

## Profiles and remotes

Profiles select which daemon a client talks to:

```toml
[profiles.work]
daemon = { kind = "remote", url = "wss://host.example:6810", secret_ref = "work" }
```

Select with `dman --profile work list` or `DMAN_PROFILE=work`. See
[Remote use](./remote) for TLS and SSH tunnels.

## Free-space guard

Two thresholds on the profile watch the free space of the download volume. Each
is an absolute size (`"5GiB"`, `"500MB"`) or a percentage (`"5%"`):

```toml
[profiles.local]
min_free = "1GiB"   # below this, downloads pause and nothing new starts
warn_free = "5GiB"  # below this, the free-space figure is flagged as low
```

The low-space warning shows as a highlighted free-space figure in the TUI
status bar and the web header, and as `(low)` after `disk free` in
`dman stats`. Changes to either threshold need a daemon restart, like the rest
of the profile.

## Secrets

API tokens live in `~/.config/dman/secrets.toml` (mode 0600; on Unix dman refuses looser
permissions). Tokens are never logged. Treat them like a password: anyone holding one can
add, remove and read downloads. To use or rotate a token, see [Remote use](./remote#the-token).

## Speed limits and the queue

- `dman limit --download 5MiB` sets the global speed limits; per-download limits are set at
  `dman add --max-download-limit 2MiB` or `dman set 12 --max-download-limit 1MiB`.
- `dman max-concurrent 5` sets how many downloads run at once.
- `[[schedule]]` tables in `config.toml` define windows in local time that run with their own
  limits or pause everything; `dman schedule` shows the windows, the active one and the next
  change. The generated config file has a commented example.

## Torrents

Torrents stop seeding when they finish unless:

```toml
[torrents]
seed = true
```

Per-download seeding rules are set with `--seed-ratio` and `--seed-time`; giving a download
one turns seeding on for it. `seed_ratio` and `seed_time_minutes` set the global stop rules.

## Notifications and hooks

Desktop notifications are on by default for `completed`, `failed` and `seed_completed`.
`[notify.webhook]` POSTs a JSON body to a URL (signed with HMAC-SHA256 when `secret_ref` is
set) and `[[notify.hook]]` runs a command, given as an argument list without a shell, with the
event in `DMAN_*` environment variables. Both are commented examples in the generated file.

## Video

`[video]` sets the paths of `yt-dlp` and `ffmpeg` (found on `PATH` by default), the default
quality, audio and container, subtitles and SponsorBlock categories. The quality picker is
shown before a video download unless you turn it off with
`dman video prefs --ask false` (see [CLI](./cli#adding-downloads)).

