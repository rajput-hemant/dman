# Remote use

The daemon owns all state; the TUI, CLI and web UI are clients of one JSON-RPC API
(`/api/v1/ws`, WebSocket). By default it listens on `127.0.0.1:6810` and the web UI is off.
To use a daemon on another machine, copy its API token into the client's `secrets.toml`, add
a `kind = "remote"` profile, and reach the daemon through an SSH tunnel (simplest), built-in
TLS or a reverse proxy. Without TLS or a tunnel the token and all traffic cross the network
in clear text.

## Choosing an option

| Option | Needs | Encryption | Best for |
| --- | --- | --- | --- |
| SSH tunnel | SSH access, no dman configuration | SSH | Quick personal access |
| Built-in TLS | Certificate and key (public CA or self-signed) | `wss://` / `https://` | A daemon on a LAN or server |
| Reverse proxy | A TLS proxy (nginx, Caddy) in front of the web UI | Proxy | Web UI under an existing domain |

## The token

The daemon generates an API token on first start and stores it in
`~/.config/dman/secrets.toml` under the profile's `secret_ref` (default `daemon`). Copy the
daemon's token into the client's `secrets.toml`:

```toml
# ~/.config/dman/secrets.toml on the client
[secrets]
work = "paste-the-token-here"
```

and add a remote profile to the client's `config.toml`:

```toml
[profiles.work]
daemon = { kind = "remote", url = "wss://host.example:6810", secret_ref = "work" }
```

Then `dman --profile work list` (or set `DMAN_PROFILE=work`).

`dman add ./x.torrent` against a remote profile reads the file on the machine where you run
the command and uploads its content, so it works the same as for a local daemon. Plain
download directories always refer to the daemon's machine.

## Option 1: SSH tunnel

Keep the daemon on loopback and forward the port:

```sh
ssh -N -L 6810:127.0.0.1:6810 host
```

```toml
[profiles.work]
daemon = { kind = "remote", url = "ws://127.0.0.1:6810", secret_ref = "work" }
```

SSH provides encryption and server authentication, so `ws://` is fine here. The same works
for the web UI (`-L 6811:127.0.0.1:6811`).

## Option 2: built-in TLS

Give each listener a certificate chain and private key in PEM format; when TLS is configured
the listener serves https/wss only.

```toml
[profiles.local.daemon]
kind = "local"
listen = "0.0.0.0:6810"
secret_ref = "daemon"
tls = { cert = "/etc/dman/cert.pem", key = "/etc/dman/key.pem" }

[web]
enabled = true
bind = "0.0.0.0"
port = 6811
tls = { cert = "/etc/dman/cert.pem", key = "/etc/dman/key.pem" }
```

- **Public CA** (for example Let's Encrypt): use the full chain; clients verify against the
  bundled Mozilla root set by default.
- **Self-signed**: generate with a subject alternative name for the host, copy only
  `cert.pem` to each client and pin it with `ca = "~/.config/dman/nas-cert.pem"` in the
  profile. The `ca` key replaces the default roots entirely.
- `insecure_skip_verify = true` accepts any certificate and logs a warning on every
  connection. Use it only for quick tests.
- Binding the API beyond loopback without TLS is allowed (the token still protects it) but
  the daemon logs a warning.

## Option 3: reverse proxy for the web UI

Run the web UI on loopback and let a proxy terminate TLS. The proxy makes it reachable from
elsewhere, so turn the login on with `require_login` (dman refuses proxied requests to a
login-free UI), and set `web.base_path` if it is mounted below a path:

```nginx
location /dman/ {
    proxy_pass http://127.0.0.1:6811;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_buffering off;
    proxy_read_timeout 1h;
}
```

In Caddy, `reverse_proxy /dman/* 127.0.0.1:6811` is enough. To expose the API too, the proxy
must forward WebSocket upgrades on `/api/v1/ws`.

## Security notes

- Keep `secrets.toml` at mode 0600 (on Unix dman refuses looser permissions).
- Prefer TLS with a pinned `ca` or an SSH tunnel over plaintext. Never expose the plain
  `ws://` or `http://` listeners to the internet.
- With a login the web UI uses `HttpOnly; SameSite=Strict` session cookies; it still needs
  HTTPS when reachable beyond loopback.
