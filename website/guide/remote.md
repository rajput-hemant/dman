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
`~/.config/dman/secrets.toml` under the profile's `secret_ref` (default `daemon`); see
[Configuration](./configuration#secrets) for the file's rules. Copy the daemon's token into
the client's `secrets.toml`:

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

Give each listener a certificate chain and private key in PEM format. When TLS is configured
the listener serves https/wss only and plaintext clients are rejected.

```toml
# API listener of the profile the daemon serves
[profiles.local.daemon]
kind = "local"
listen = "0.0.0.0:6810"
secret_ref = "daemon"
tls = { cert = "/etc/dman/cert.pem", key = "/etc/dman/key.pem" }

# Web UI: a separate listener, with the same or another certificate
[web]
enabled = true
bind = "0.0.0.0"
port = 6811
tls = { cert = "/etc/dman/cert.pem", key = "/etc/dman/key.pem" }
```

The files are checked when the configuration is loaded, and the daemon refuses to start on a
missing or unusable one. A certificate change needs a daemon restart. With TLS on the web
listener the session cookie is always `Secure`.

### Certificate from a public CA

Use the full chain (`fullchain.pem`) and key, for example from Let's Encrypt. Clients trust it
out of the box: by default they verify against the bundled Mozilla root set (the system
certificate store is not read). Use the host name from the certificate in the URL.

### Self-signed certificate

Generate one with a subject alternative name for the name or address clients will use:

```sh
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
  -keyout key.pem -out cert.pem -days 825 -subj "/CN=dman" \
  -addext "subjectAltName=DNS:nas.lan,IP:192.168.1.10"
chmod 600 key.pem
```

Install both files on the daemon as above, copy **only `cert.pem`** to each client and pin it
in the client profile:

```toml
[profiles.nas]
daemon = { kind = "remote", url = "wss://nas.lan:6810", secret_ref = "nas", ca = "~/.config/dman/nas-cert.pem" }
```

`ca` replaces the default roots entirely: only that certificate (or a CA that signed the
daemon's certificate) is trusted. It is the recommended way to use a self-signed or private CA
certificate. If a certificate is not trusted the client says `certificate not trusted: ...`;
fix the `ca` path or the certificate's subject alternative names.

A local profile with `daemon.tls` set reaches its own daemon at its listen address
(`wss://127.0.0.1:6810`) and trusts the configured certificate file, so that certificate needs
`IP:127.0.0.1` (or the listen address) among its subject alternative names. With a public CA
certificate, use a remote profile with the real host name for the local CLI instead.

### Skipping verification

```toml
daemon = { kind = "remote", url = "wss://host:6810", secret_ref = "work", insecure_skip_verify = true }
```

This accepts any certificate, including an attacker's, and logs a warning on every
connection. Use it only for quick tests; prefer `ca`.

### Plaintext listeners

Binding the API beyond loopback without TLS is allowed (the token still protects it), but the
daemon logs a warning. Use TLS or an SSH tunnel.

## Option 3: reverse proxy for the web UI

Run the web UI on loopback and let a proxy terminate TLS. The proxy makes it reachable from
elsewhere, so turn the login on with `require_login`: dman refuses proxied requests
(`X-Forwarded-For`, `Forwarded`, ...) to a login-free UI. If the UI is mounted below a path,
set `base_path`:

```toml
[web]
enabled = true
bind = "127.0.0.1"
port = 6811
base_path = "/dman"
require_login = true
```

An nginx location (the page uses a long-lived event stream, so buffering is off and the read
timeout is raised):

```nginx
location /dman/ {
    proxy_pass http://127.0.0.1:6811;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_buffering off;
    proxy_read_timeout 1h;
}
```

`X-Forwarded-Proto: https` makes the session cookie `Secure`. In Caddy,
`reverse_proxy /dman/* 127.0.0.1:6811` is enough. To expose the API too, the proxy must
forward WebSocket upgrades on `/api/v1/ws`. The web UI's own token and access rules are on the
[Web UI](./web-ui) page.

## Security notes

- A token is a password: anyone holding it can add, remove and read downloads. Keep
  `secrets.toml` at mode 0600 and rotate a token by editing it on both sides and restarting
  the daemon.
- Prefer TLS with a pinned `ca` or an SSH tunnel over plaintext. Never expose the plain
  `ws://` or `http://` listeners to the internet.
- `insecure_skip_verify` defeats the purpose of TLS.
- With a login the web UI uses `HttpOnly; SameSite=Strict` session cookies; it still needs
  HTTPS when reachable beyond loopback.
- The TUI reconnects automatically after a network drop; see [TUI](./tui#disconnects).
