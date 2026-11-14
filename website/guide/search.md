# Search

dman searches torrent sites through *providers*: declarative definitions (a URL plus how to
read the answer). Every built-in provider is off until you enable it.

```bash
dman providers list                 # state, verified/unverified, mirrors, proxy
dman providers enable nyaa eztv
dman providers test eztv            # one live request; shows which mirror answered
dman search "big buck banana"
dman search ubuntu --add 1,2        # add results by their numbers
```

## Built-in providers

The Pirate Bay, Nyaa, Knaben, YTS, EZTV, Torlock, TorrentDownloads, Internet Archive,
Academic Torrents, LinuxTracker, LimeTorrents, SolidTorrents, 1337x and a Torznab template
(Prowlarr / Jackett).

- `verified` means the definition was checked against the live site and a real response is
  kept as a test fixture.
- `unverified` means it was written from the public format only; try
  `dman providers test NAME` before relying on it.
- Sites move and change their markup: when a provider breaks, adjust it in `config.toml`.

## Search options

`dman search` filters by provider and category:

```bash
dman search 'blade runner' --provider YTS --category movies
dman search ubuntu --limit 5 --no-prompt      # never ask which to add (default in scripts)
dman search --history                          # recent queries; --clear-history forgets them
```

In a terminal, results are followed by "Add which?"; answer with numbers such as `1,3` or
`2-4`, Enter skips.

## Mirrors

A provider can list other addresses for the same site. dman keeps the path and query of the
provider's URL and swaps the host. On a connection error, timeout, HTTP 403, 451 or 5xx, or
an anti-bot challenge page, the next mirror is tried within the provider's `timeout_secs`.
The mirror that worked last is asked first for the rest of the session.

```toml
[[search.provider]]
name = "Nyaa"                       # a built-in: only the changed keys are needed
mirrors = ["https://nyaa.land", "https://nyaa.example"]
```

## Proxy

```toml
[search]
proxy = "socks5h://127.0.0.1:9050"  # all providers and the tracker list

[[search.provider]]
name = "YTS"
proxy = "http://user:pass@proxy.example:3128"   # this provider only

[[search.provider]]
name = "Knaben"
proxy = "none"                      # this provider connects directly
```

`http`, `https`, `socks5` and `socks5h` (names resolved on the proxy) are supported; `none`
connects directly. A provider's own `proxy` wins over `[search] proxy`. Credentials are
hidden in `dman providers list`, errors and debug output, and `dman providers export` leaves
the proxy out. Search proxies do not affect aria2.

## DNS over HTTPS

Some countries block sites only at the DNS level. dman can resolve provider and tracker-list
host names through DNS over HTTPS instead of the system resolver:

```toml
[search]
dns = "doh:cloudflare"              # or "doh:quad9", "doh:https://dns.example/dns-query", "system"
```

`doh:cloudflare` and `doh:quad9` use fixed IP addresses, so no system lookup is needed to
reach them. This covers provider requests and the tracker list only; it does not apply to
aria2.

## Cloudflare-protected sites (1337x)

1337x and a few others answer scripted clients with a Cloudflare challenge. dman does not try
to solve challenges; the supported route is an indexer manager that does, used through the
Torznab provider:

1. Run Prowlarr (or Jackett) and FlareSolverr.
2. Add the 1337x indexer to the manager.
3. Put the manager's API key in `secrets.toml` under `[secrets] torznab = "..."`.
4. Point the built-in Torznab provider at the manager and enable it:

   ```toml
   [[search.provider]]
   name = "Torznab"
   enabled = true
   url = "http://localhost:9696/api/v2.0/indexers/all/results/torznab/api"   # Prowlarr
   api_key_ref = "torznab"
   ```

   then `dman config reload` and `dman providers test torznab`.

## Custom providers

Add a complete `[[search.provider]]` table with a new name. Three kinds:

| `kind` | Source | Fields |
| --- | --- | --- |
| `feed` | RSS / Atom | `url` (`{query}` optional) |
| `json` | JSON API | `url`, `items`, `title`, `size`, `seeders`, `leechers`, `info_hash` (JSON pointers) |
| `html` | HTML page | `search_url`, `row`, `title`, `detail_link`, `info_hash`, `size`, `seeders`, `leechers` (CSS selectors) |

```toml
[[search.provider]]
name = "My feed"
kind = "feed"
url = "https://tracker.example/rss?q={query}"
```

Useful extra keys on any provider: `mirrors`, `proxy`, `headers`, `timeout_secs`,
`rate_limit_per_min`, `max_results`, `test_query` and `local_filter = true` for an endpoint
that only lists recent items. Share one with `dman providers export NAME > mine.toml` and
load it with `dman providers import mine.toml` (imported providers start disabled and
unverified).
