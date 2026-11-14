---
layout: home

hero:
  name: dman
  tagline: A terminal-first download manager for torrents, magnet links, Metalink, HTTP/FTP/SFTP and video sites, with a full-screen TUI, a web UI and a scriptable CLI in one binary.
  actions:
    - theme: brand
      text: Getting started
      link: /guide/getting-started
    - theme: alt
      text: Install
      link: /guide/install

features:
  - title: Every protocol
    details: Magnets with file selection, torrents, Metalink, HTTP/FTP/SFTP and video sites, driven by aria2c and yt-dlp.
    link: /guide/getting-started
  - title: One daemon, three clients
    details: The TUI, web UI and CLI are clients of one JSON-RPC-over-WebSocket API, and any of them can also control a remote daemon.
    link: /guide/tui
  - title: Scriptable CLI
    details: Add, inspect, control and export JSON for scripts. Tab completion for IDs, providers and profiles.
    link: /guide/cli
  - title: Full-screen TUI
    details: Queue with sort and filter, details, pause and resume, a command palette and all keys under a question mark.
    link: /guide/tui
  - title: Web UI and browser hooks
    details: dman serve opens a browser UI on your daemon; dman integrate install makes magnet links and .torrent files open in dman.
    link: /guide/web-ui
  - title: Built-in search
    details: Torrent search over configurable providers, mirrors, proxies and Torznab for Cloudflare-protected sites. Off until you enable it.
    link: /guide/search
---
