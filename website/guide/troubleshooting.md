# Troubleshooting

Common failures, in the words dman itself uses.

## Nothing downloads and the engine shows as unavailable

`aria2c` (1.36+) is required. Install it with your package manager (Homebrew, Scoop, winget
and the AUR do it together with dman), then:

```bash
dman daemon engine-restart
```

Without `yt-dlp`, video downloads are disabled; other URLs download as usual and `--no-video`
forces a plain download of a page or file. Without `ffmpeg`, dman picks the best single-file
quality instead; `dman probe URL` shows what is possible.

## The daemon cannot be reached (exit code 3)

The clients talk to a background daemon on `127.0.0.1:6810`. Start it and check status:

```bash
dman daemon start
dman daemon status
```

If it ran before, `dman daemon status` shows whether it runs and which engines it found.

## A provider search fails

- Every provider is off until you enable one: `dman providers enable nyaa`.
- Providers marked *unverified* were written from the public format and may not work. Run
  `dman providers test NAME` and read what it reports (HTTP status, challenge page, selector
  that matched nothing).
- A Cloudflare challenge page means dman will not fetch the site; use the Torznab provider
  with Prowlarr or Jackett (see [Search](./search)).
- For blocked sites, set a `proxy` or DNS over HTTPS in `[search]` (see
  [Search](./search)).

## A download is refused with exit code 9

The link (same URL or torrent info hash) is already in your downloads. The message names the
existing job; use `--force` to add anyway. A duplicate torrent receives the new link's
trackers instead. `dman remove 12` followed by `dman restore 12` undoes a remove.

## dman cannot read the clipboard

`dman add --clipboard` needs a clipboard tool: `wl-paste` (Wayland), `xclip` or `xsel` (X11),
`pbpaste` (macOS) or PowerShell's `Get-Clipboard` (Windows). Install one, then retry.

## A setting did not take effect

```bash
dman config reload
```

Some settings need a restart of the daemon: `dman daemon restart`. Check the effective
values with `dman config show` and a single one with `dman config get limits.download`.

## Wrong token or unauthorized (exit code 4)

The token lives in `~/.config/dman/secrets.toml` (mode 0600). Copy the daemon's token into
the client's file and restart the daemon after rotating. See [Remote use](./remote).

## A remote connection fails

- `certificate not trusted: ...` means the client does not trust the daemon's certificate.
  Fix the `ca` path in the profile or the certificate's subject alternative names.
- The daemon refused to start: TLS files are checked when the configuration is loaded; a
  missing or unusable certificate or key stops it.
- `dman daemon status` shows daemon and engine state; `dman --json` turns any error into a
  machine-readable document with a code.
