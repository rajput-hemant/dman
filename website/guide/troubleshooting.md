# Troubleshooting

Common failures, in the words dman itself uses.

## Nothing downloads and the engine shows as unavailable

`aria2c` (1.36+) is missing or not found. Install it with your package manager, then:

```bash
dman daemon engine-restart
```

For `yt-dlp` and `ffmpeg`, which are optional, see
[Requirements](./getting-started#requirements).

## The daemon cannot be reached (exit code 3)

The clients talk to a background daemon on `127.0.0.1:6810`. Start it and check status:

```bash
dman daemon start
dman daemon status
```

`dman daemon status` shows whether it runs and which engines it found.

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
existing job; use `--force` to add anyway. See [CLI](./cli#adding-downloads).

## A Google Drive folder is refused

- `folder must be public or link-shared` or `folder page missing initial view data ... the
  folder may be private`: share the folder as "anyone with the link". dman does not sign in
  to Google.
- `quota exceeded`: Google limits downloads of popular files for a while. Retry later.
- `folder contains no files`: the folder, with its subfolders, has nothing to download.
- `no new jobs: every file in the folder is already queued`: everything is in the list
  already; use `--force` to add the files again.

See [CLI](./cli#google-drive-folders).

## dman cannot read the clipboard

`dman add --clipboard` needs a clipboard tool. See [Browser](./browser#clipboard) for the one
to install on your platform.

## A setting did not take effect

```bash
dman config reload
```

Some settings need `dman daemon restart` instead; see
[Configuration](./configuration#reading-and-changing-settings). Check the effective values with
`dman config show`.

## Wrong token or unauthorized (exit code 4)

The client's token does not match the daemon's. Copy the daemon's token into the client's
`secrets.toml` as described in [Remote use](./remote#the-token).

## A remote connection fails

- `certificate not trusted: ...` means the client does not trust the daemon's certificate.
  Fix the `ca` path in the profile or the certificate's subject alternative names.
- The daemon refused to start: TLS files are checked when the configuration is loaded; a
  missing or unusable certificate or key stops it.
- `dman daemon status` shows daemon and engine state; `dman --json` turns any error into a
  machine-readable document with a code.
