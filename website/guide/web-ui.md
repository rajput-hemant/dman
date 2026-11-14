# Web UI

`dman serve` turns the browser UI on in the running daemon; `--persist` keeps it on.

```bash
dman serve                          # http://127.0.0.1:6811/
dman serve --bind 0.0.0.0 --persist # expose it on the network and remember that
dman serve --base-path /dman        # behind a reverse proxy at /dman
dman serve --stop                   # turn the web UI off again
```

## Access model

| Setting | Behavior |
| --- | --- |
| Default address | `127.0.0.1:6811`, no login |
| Network address (`--bind 0.0.0.0`) | Requires an access token; `--print-token` shows it |

On the default loopback address there is no login: you open `http://127.0.0.1:6811/` and you
are in. Other websites cannot drive it through your browser:

- state-changing requests must come from the page itself (`Origin`/`Sec-Fetch-Site` check
  plus a per-page CSRF token),
- the `Host` header must name loopback, the bind address or an entry of `web.allowed_hosts`,
  which stops DNS rebinding.

When the web UI is bound to a network address it asks for its own access token (`web` in
`secrets.toml`). Without a token the daemon refuses to serve a network address unless
`web.allow_unauthenticated = true` (every page then shows a warning banner).
`web.require_login = true` asks for the token on loopback too.

## `[web]` keys

| Key | Effect |
| --- | --- |
| `enabled` | Start the web UI with the daemon (default `false`; `dman serve` turns it on) |
| `bind` | Address to bind (default `127.0.0.1`; anything but loopback exposes the UI) |
| `port` | TCP port, separate from the API port (default `6811`) |
| `title` | Header and tab title |
| `tls` | Serve https with this certificate; plain http when absent |
| `token_ref` | Name of the `secrets.toml` entry holding the access token (default `web`) |
| `allowed_hosts` | Extra `Host` header names accepted on a login-free UI |
| `allow_unauthenticated` | Serve a network address without a token (default `false`) |
| `require_login` | Ask for the token on loopback too (default `false`) |
| `base_path` | Mount point behind a reverse proxy (default `/`) |

## Behind a proxy or over TLS

For TLS on the web listener, reverse-proxy examples and token handling, see
[Remote use](./remote).
