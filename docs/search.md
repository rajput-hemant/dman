# Search providers

**TL;DR:** dman searches torrent sites through *providers*: declarative definitions (a URL plus
how to read the answer). Every built-in provider is off until you enable it. Fastest path:
`dman providers enable nyaa`, then `dman search "your words"`.

## Contents

- [Quick start](#quick-start)
- [Built-in providers](#built-in-providers)
- [Mirrors](#mirrors)
- [Proxy](#proxy)
- [DNS over HTTPS](#dns-over-https)
- [Cloudflare-protected sites (1337x)](#cloudflare-protected-sites-1337x)
- [Custom providers](#custom-providers)

## Quick start

```bash
dman providers list                 # state, verified/unverified, mirrors, proxy
dman providers enable nyaa eztv
dman providers test eztv            # one live request; shows which mirror answered
dman search "big buck bunny"
```

`verified` means the definition was checked against the live site and a real response is
kept as a test fixture. `unverified` means it was written from the public format only; try
`dman providers test NAME` before relying on it. Sites move and change their markup: when a
provider breaks, adjust it in `config.toml` (see [Custom providers](#custom-providers)).

## Built-in providers

| Provider | Method | Status |
| --- | --- | --- |
| The Pirate Bay | apibay.org JSON API | verified |
| Nyaa | RSS | verified; mirror nyaa.land |
| Knaben | JSON API (meta-search) | verified |
| YTS | JSON API (movies-api.accel.li) | verified; mirrors yts.gg, yts.lt, yts.am, yts.mx |
| EZTV | JSON API | verified; mirrors eztv.re, eztv.wf, eztv.tf. The API cannot search by text: it lists the newest 100 TV torrents and dman keeps the ones that match your words |
| Torlock | RSS (newest 30) | verified; filtered by your words like EZTV (the search pages sit behind a Cloudflare challenge) |
| TorrentDownloads | HTML, magnet from the detail page | verified |
| Internet Archive | JSON advancedsearch | verified; free and public-domain items, one `.torrent` per item, no seeder counts |
| Academic Torrents | RSS (newest 30) | verified; data sets and course material; filtered by your words (the site asks scrapers to use its XML files, and the full database file is too large) |
| LinuxTracker | HTML, hash from the link | verified |
| LimeTorrents | HTML, magnet from the detail page | unverified: its search page did not answer from the test machine; selectors come from the real "latest" and detail pages. Mirror limetorrents.lol |
| SolidTorrents | JSON API | unverified: bitsearch.to (where solidtorrents.to now points) answered 500 or timed out; format from the API's earlier documentation. Mirrors solidtorrents.to, solidtorrents.net |
| 1337x | HTML | unverified: Cloudflare challenge, see [below](#cloudflare-protected-sites-1337x); mirrors 1337x.st, 1337xx.to, x1337x.ws |
| Torznab | Prowlarr / Jackett | unverified template, see [below](#cloudflare-protected-sites-1337x) |

## Mirrors

A provider can list other addresses for the same site:

```toml
[[search.provider]]
name = "Nyaa"                       # a built-in: only the changed keys are needed
mirrors = ["https://nyaa.land", "https://nyaa.example"]
```

Mirrors are base URLs (`https://host[:port]`, no path). dman keeps the path and query of the
provider's URL and swaps the host. If the main host fails with a connection error, a timeout,
HTTP 403, 451 or 5xx, or answers with an anti-bot challenge page ("Just a moment...",
"Attention Required", DDoS-Guard), the next mirror is tried within the provider's
`timeout_secs`; each attempt gets a share of the time left, so one dead host cannot use it all.
The mirror that worked last is asked first for the rest of the session. Links to result pages
on the main host follow the mirror that answered, and detail pages (for magnets) fall back to
the other mirrors in the same way. `dman providers test NAME` and `dman search` say which mirror
answered. Other errors, such as a 404 or markup that no longer matches, are reported as they
are: a different host would not fix them.

`dman providers list` shows how many mirrors each provider has. Setting `mirrors` on a built-in
replaces its list.

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

| Scheme | Notes |
| --- | --- |
| `http`, `https`, `socks5` | Standard proxy URLs |
| `socks5h` | Resolves names on the proxy, which also gets around DNS blocks |
| `none` | Connect directly (per provider) |

A provider's own `proxy` wins over
`[search] proxy`. Credentials in a proxy URL are hidden in `dman providers list`, in error
messages and in debug output, and `dman providers export` leaves the proxy out of the file.
Search proxies do not affect aria2; give aria2 its own proxy in the per-download options.

## DNS over HTTPS

Some countries block sites only at the DNS level. dman can resolve provider and
tracker-list host names through DNS over HTTPS instead of the system resolver:

```toml
[search]
dns = "doh:cloudflare"              # or "doh:quad9", "doh:https://dns.example/dns-query", "system"
```

| Value | Resolver |
| --- | --- |
| `system` | The system resolver (default behavior) |
| `doh:cloudflare` | 1.1.1.1 / 1.0.0.1 |
| `doh:quad9` | 9.9.9.9 / 149.112.112.112 |
| `doh:https://host/dns-query` | A custom endpoint |

`doh:cloudflare` and `doh:quad9` use fixed IP addresses (1.1.1.1 / 1.0.0.1 and 9.9.9.9 /
149.112.112.112), so no system lookup is needed to reach them. A custom `doh:https://host/...`
URL with a host name is looked up with the system resolver once, so prefer an IP address when
the system DNS is what you are avoiding. Answers are cached for their TTL, at most five minutes.

Limits: this covers provider requests and the tracker list only. Requests through an `http(s)`
or `socks5h` proxy are resolved by the proxy, so the setting does not apply to them. It does
not apply to aria2 either: download connections use aria2's own resolver, which has the
`async-dns-server` option (set it in the aria2 options, for example `async-dns-server=1.1.1.1`).

## Cloudflare-protected sites (1337x)

1337x and a few others answer scripted clients with a Cloudflare challenge. dman does not try
to solve challenges. The supported route is an indexer manager that does, used through the
Torznab provider:

1. Run Prowlarr (or Jackett) and FlareSolverr (`docker run -p 8191:8191
   ghcr.io/flaresolverr/flaresolverr`).
2. In Prowlarr add FlareSolverr under Settings, Indexers (host `http://localhost:8191`) with a
   tag, add the 1337x indexer and give it the same tag. In Jackett set the FlareSolverr API URL
   in its server settings and add the indexer.
3. Put the API key in `secrets.toml` (mode 0600):

   ```toml
   [secrets]
   torznab = "paste-the-api-key-here"
   ```

4. Point the built-in Torznab provider at the manager and enable it:

   ```toml
   [[search.provider]]
   name = "Torznab"
   enabled = true
   url = "http://localhost:9696/api/v2.0/indexers/all/results/torznab/api"   # Prowlarr
   # url = "http://localhost:9117/api/v2.0/indexers/all/results/torznab/"    # Jackett
   api_key_ref = "torznab"
   ```

   then `dman config reload` and `dman providers test torznab`.

The built-in 1337x provider stays unverified: it is kept for networks and mirrors where it
answers, with the mirrors 1337x.st, 1337xx.to and x1337x.ws.

## Custom providers

Add a complete `[[search.provider]]` table with a new name (a built-in needs only the keys you
change).

| `kind` | Source | Fields |
| --- | --- | --- |
| `feed` | RSS / Atom | `url` (`{query}` optional) |
| `json` | JSON API | `url`, `items`, `title`, `size`, `seeders`, `leechers`, `info_hash` (JSON pointers) |
| `html` | HTML page | `search_url`, `row`, `title`, `detail_link`, `info_hash`, `size`, `seeders`, `leechers` (CSS selectors) |

Examples:

```toml
# RSS / Atom, e.g. a tracker's search feed ({query} is optional)
[[search.provider]]
name = "My feed"
kind = "feed"
url = "https://tracker.example/rss?q={query}"

# JSON API: JSON pointers pick the fields, {..} builds links
[[search.provider]]
name = "My API"
kind = "json"
url = "https://api.example/search?q={query}"
items = "/results"
title = "/name"
size = "/bytes"
seeders = "/seeds"
leechers = "/leeches"
info_hash = "/hash"                 # a magnet is built from it; or set `magnet` / `torrent_url`

# HTML page: CSS selectors per row, `selector @attribute` reads an attribute
[[search.provider]]
name = "My site"
kind = "html"
search_url = "https://site.example/search?q={query}"
row = "table.results tr"
title = "a.title"
detail_link = "a.title @href"
info_hash = "a.title @href"         # the 40-hex hash may sit inside a link or cell
size = "td.size"
seeders = "td.seeds"
leechers = "td.leech"
```

Useful extra keys on any provider: `mirrors`, `proxy`, `headers` (for example a cookie),
`timeout_secs`, `rate_limit_per_min`, `max_results`, `test_query` and `local_filter = true` for
an endpoint that only lists recent items (dman filters the results by your words). An HTML
provider whose rows have no magnet sets `follow_detail_for_magnet = true` and `detail_magnet`
(selector on the detail page). `dman providers test NAME` makes one request and reports what
went wrong in plain words (HTTP status, challenge page, selector that matched nothing).

Share a provider with `dman providers export NAME > mine.toml` and load one with
`dman providers import mine.toml` (imported providers start disabled and unverified).
