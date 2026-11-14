# Configuration

The config file is `~/.config/dman/config.toml` (`dman config path` prints the location;
`DMAN_CONFIG_DIR` relocates everything). `dman daemon gen-config` writes a commented file to
edit; defaults apply without a file.

```bash
dman daemon gen-config   # write the default config file if none exists
dman config edit         # edit in $EDITOR; checked before saving
dman config reload       # apply to the running daemon
```

## Reading and changing settings

| Command | Purpose |
| --- | --- |
| `dman config show` | Effective settings, secrets redacted |
| `dman config get limits` | One setting or a section |
| `dman config set limits.download 5MiB` | Change a setting; comments and layout are kept |
| `dman config edit` | Edit in `$EDITOR`; checked before saving |
| `dman config reload` | Apply to the running daemon |

Some settings need a daemon restart: `dman daemon restart`, for example after changing a
listener address.

## Profiles and remotes

Profiles select which daemon a client talks to:

```toml
[profiles.work]
daemon = { kind = "remote", url = "wss://host.example:6810", secret_ref = "work" }
```

Select with `dman --profile work list` or `DMAN_PROFILE=work`. See
[Remote use](./remote) for tokens, TLS and SSH tunnels.

## Secrets

API tokens live in `~/.config/dman/secrets.toml` (mode 0600; on Unix dman refuses looser
permissions). Tokens are never logged. Treat them like a password: anyone holding one can
add, remove and read downloads. Rotate by editing the file on both sides and restarting the
daemon.

## Speed limits and the queue

- `dman limit --download 5MiB` sets the global speed limits; per-download limits are set at
  `dman add --max-download-limit 2MiB` or `dman set 12 --max-download-limit 1MiB`.
- `dman max-concurrent 5` sets how many downloads run at once.
- `[[schedule]]` tables in `config.toml` define schedule windows; `dman schedule` shows the
  windows, the active one and the next change.

## Torrents

Torrents stop seeding when they finish unless:

```toml
[torrents]
seed = true
```

Per-download seeding rules are set with `--seed-ratio` and `--seed-time`.

## Search settings

The `[search]` section and `[[search.provider]]` tables control providers, mirrors, proxies
and DNS over HTTPS: see [Search](./search).
