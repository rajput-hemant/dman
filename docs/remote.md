# Remote operation

The dman daemon owns all state; the TUI, CLI and web UI are clients of one JSON-RPC API
(`/api/v1/ws`, WebSocket). By default it listens on `127.0.0.1:6810` and the web UI is off.
To use a daemon on another machine you have three options, from simplest to most flexible:

1. an SSH tunnel (no dman configuration at all),
2. built-in TLS (`wss://` / `https://`) with your own certificate,
3. a TLS reverse proxy in front of the web UI.

Every API connection is authenticated with a token. Without TLS or a tunnel the token and all
traffic cross the network in clear text. The web UI asks for a token only when it is reachable
from the network (see below).

## The token

The daemon generates an API token on first start and stores it in `secrets.toml`
(mode 0600) under the profile's `secret_ref` (default `daemon`). Tokens are never logged.

### The web UI token

On its default loopback address (`127.0.0.1` or `::1`) the web UI has **no login**: open
`http://127.0.0.1:6811/` and you are in, and no web token is created. Other websites still
cannot drive it through your browser: every state-changing request must come from the page
itself (`Origin`/`Sec-Fetch-Site` check plus a per-page CSRF token that the page sends on its
own), and the `Host` header must name loopback, the bind address or an entry of
`web.allowed_hosts`, which stops DNS-rebinding attacks. Requests relayed by a proxy
(`X-Forwarded-For`, `Forwarded`, ...) are refused on a login-free loopback UI.

When the web UI is bound to a network address it asks for its own access token (`web` in
`secrets.toml`, see `web.token_ref`). `dman serve --bind 0.0.0.0` generates it on first use and
`dman serve --print-token` shows it. Without a token the daemon refuses to serve a network
address unless `web.allow_unauthenticated = true` (then every page shows a warning banner).
`web.require_login = true` asks for the token on loopback too.

To reach a daemon from another machine, copy its token into that machine's `secrets.toml`:

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

File paths: `dman add ./x.torrent` against a remote profile reads the file on the machine where
you run the command and uploads its content, so it works the same as for a local daemon. Plain
download directories always refer to the daemon's machine.

## Option 1: SSH tunnel

Keep the daemon on loopback and forward the port:

```sh
ssh -N -L 6810:127.0.0.1:6810 host
```

The client then uses an ordinary local-looking profile:

```toml
[profiles.work]
daemon = { kind = "remote", url = "ws://127.0.0.1:6810", secret_ref = "work" }
```

SSH provides encryption and server authentication, so `ws://` is fine here. The same works
for the web UI (`-L 6811:127.0.0.1:6811`).

## Option 2: built-in TLS

dman uses rustls (ring crypto provider). Give each listener a certificate chain and private
key in PEM format; when TLS is configured the listener serves https/wss only and plaintext
clients are rejected.

```toml
# API listener of the profile the daemon serves
[profiles.local.daemon]
kind = "local"
listen = "0.0.0.0:6810"
secret_ref = "daemon"
tls = { cert = "/etc/dman/cert.pem", key = "/etc/dman/key.pem" }

# Web UI (separate listener, can use the same or another certificate)
[web]
enabled = true
bind = "0.0.0.0"
port = 6811
tls = { cert = "/etc/dman/cert.pem", key = "/etc/dman/key.pem" }
```

The files are read and checked when the configuration is loaded and when the listener starts;
the daemon refuses to start on a missing or unusable file. A certificate change needs a
daemon restart. With TLS on the web listener the session cookie is always `Secure`.

### Certificate from a public CA

Use the full chain (`fullchain.pem`) and key (e.g. from Let's Encrypt). Clients trust it out
of the box: by default they verify against the bundled Mozilla root set (`webpki-roots`, no
system certificate store is read). Use the host name from the certificate in the URL.

### Self-signed certificate

Generate one with a subject alternative name for the name or address clients will use:

```sh
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
  -keyout key.pem -out cert.pem -days 825 -subj "/CN=dman" \
  -addext "subjectAltName=DNS:nas.lan,IP:192.168.1.10"
chmod 600 key.pem
```

Install `cert.pem` and `key.pem` on the daemon as above and copy **only `cert.pem`** to each
client, then pin it in the client profile:

```toml
[profiles.nas]
daemon = { kind = "remote", url = "wss://nas.lan:6810", secret_ref = "nas", ca = "~/.config/dman/nas-cert.pem" }
```

`ca` replaces the default roots entirely: only that certificate (or a CA that signed the
daemon's certificate) is trusted. This is the recommended way to use a self-signed or private
CA certificate. If the certificate is not trusted the client says so
(`certificate not trusted: ...`); fix the `ca` path or the certificate's subject alternative
names.

A local profile with `daemon.tls` set reaches its own daemon at its listen address
(`wss://127.0.0.1:6810`) and trusts the configured certificate file; for this the certificate
needs `IP:127.0.0.1` (or the listen address) among its subject alternative names. With a public
CA certificate, use a remote profile with the real host name for the local CLI instead.

### Skipping verification (not recommended)

```toml
daemon = { kind = "remote", url = "wss://host:6810", secret_ref = "work", insecure_skip_verify = true }
```

This accepts any certificate, including an attacker's, and logs a warning on every connection.
Use it only for quick tests; prefer `ca`.

### Exposure warnings

Binding the API beyond loopback without TLS is allowed (the token still protects it) but the
daemon logs a warning recommending TLS or an SSH tunnel. A non-loopback web UI additionally
refuses to start without an access token unless `web.allow_unauthenticated = true`. A loopback
web UI needs no token at all.

## Option 3: reverse proxy for the web UI

Run the web UI on loopback and let a proxy terminate TLS. The proxy makes it reachable from
elsewhere, so turn the login on with `require_login` (dman refuses proxied requests to a
login-free UI). If it is mounted below a path, set `web.base_path`:

```toml
[web]
enabled = true
bind = "127.0.0.1"
port = 6811
base_path = "/dman"
require_login = true
```

Example nginx location (the page uses a long-lived event stream, so buffering is off and the read
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
`reverse_proxy /dman/* 127.0.0.1:6811` is enough (it sets the header itself). To expose the API (not just the web UI) through a proxy, it must forward
WebSocket upgrades on `/api/v1/ws`.

## Security notes

- Treat the token like a password: anyone holding it can add downloads, delete files the user
  asked to delete, and read the download list. Keep `secrets.toml` at mode 0600 (dman refuses
  looser permissions).
- Prefer TLS with a pinned `ca` or an SSH tunnel over plaintext. Never expose the plain
  `ws://` or `http://` listeners to the internet.
- `insecure_skip_verify` defeats the purpose of TLS.
- The web UI checks `Origin`, CSRF tokens and (without a login) the `Host` header on every
  request; with a login it uses `HttpOnly; SameSite=Strict` cookies. It still needs HTTPS when
  reachable beyond loopback.
- Rotate a token by editing `secrets.toml` on both sides and restarting the daemon.
- Reconnection: the TUI and other long-lived clients reconnect automatically with backoff after
  a network drop or daemon restart, show the state as disconnected, disable actions meanwhile
  (calls fail immediately, nothing is queued) and re-sync from a fresh snapshot once connected.
