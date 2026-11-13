# Sending links from the browser to dman

**TL;DR:** there are three ways to get a magnet link, a `.torrent` file or a download URL from
the browser into dman. None of them weakens the web UI's protection against other websites.
Fastest path: run `dman integrate install` (Linux, Windows) so magnet links open in dman.

## Contents

- [Handler for magnet links and .torrent files](#handler-for-magnet-links-and-torrent-files)
- [Bookmarklet and userscript](#bookmarklet-and-userscript-web-ui)
- [Clipboard](#clipboard)
- [Duplicates](#duplicates)

## Comparison

| Way | Needs | Confirms before adding |
| --- | --- | --- |
| Make dman the handler of magnet links and `.torrent` files | `dman integrate install` (Linux, Windows) | no, adds at once and shows a notification |
| Bookmarklet or userscript that opens the web UI's add page | the web UI (`dman serve`) on this computer | yes, you press **Add** |
| Copy the link, then run `dman add` | a clipboard tool | yes, in the terminal |

## Handler for magnet links and .torrent files

```sh
dman integrate install      # register dman and make it the default
dman integrate status       # which program opens magnet links now
dman integrate uninstall    # remove what install wrote, nothing else
```

The browser (or file manager) then runs `dman handle <link-or-file>`. It starts the daemon
when needed, adds the link with your default options and shows a desktop notification with
the result, because there is usually no terminal to print to. A link that is already in your
downloads is not added again; for a torrent its new trackers are added to the existing job.

Per platform:

| Platform | Mechanism |
| --- | --- |
| Linux | A `.desktop` file plus `xdg-mime default` |
| Windows | Registry keys under `HKEY_CURRENT_USER\Software\Classes` |
| macOS | Manual: `install` prints the steps |

- **Linux**: `install` writes `~/.local/share/applications/dman-handler.desktop`
  (`$XDG_DATA_HOME` is honoured) for `x-scheme-handler/magnet` and
  `application/x-bittorrent`, makes it the default with `xdg-mime default`, and runs
  `update-desktop-database` when it is installed. Without `xdg-mime`, choose "dman" in your
  desktop's default-applications settings. `uninstall` deletes that file only if its
  content shows dman wrote it; your `mimeapps.list` is left alone, so pick another default
  afterwards if you like.
- **Windows**: `install` writes, with `reg.exe`, under `HKEY_CURRENT_USER\Software\Classes`:
  `magnet` (`URL Protocol`, `shell\open\command` = `"<path>\dman.exe" handle "%1"`),
  `dman.torrent` (the same command) and the default value of `.torrent` = `dman.torrent`.
  `uninstall` deletes those keys only when their command is dman's.
- **macOS**: a URL scheme can only be registered by an application bundle, so dman does not
  pretend to do it from the command line. `dman integrate install` prints the steps: an
  AppleScript application whose `on open location` handler runs `dman handle`, with
  `CFBundleURLTypes` (`magnet`) and `CFBundleDocumentTypes` (`torrent`) in its
  `Info.plist`, made the default with a tool such as `duti`.

## Bookmarklet and userscript (web UI)

The web UI refuses state-changing requests from other websites: they must come from its own
pages (`Origin`/`Sec-Fetch-Site` check) and carry the per-page CSRF token. A bookmarklet runs
in the context of the site you are on, so it cannot (and should not be able to) post to dman
directly. What it can do is **open the add page with the link filled in**: `GET /add?uri=...`
only fills the form, and you confirm with **Add**, which the page itself sends with its own
CSRF token. Nothing is added without that click.

This works as is for the default, login-free web UI on `http://127.0.0.1:6811`. Adjust the
address if you changed `[web]` (port, `--base-path`).

Both are used with the web UI on this computer; they only prefill the add form.

### Bookmarklet

Create a bookmark with this URL. Click it on any page: it takes the selected text if it is
a link, otherwise asks, suggesting the first magnet or `.torrent` link of the page.

```js
javascript:(()=>{const d='http://127.0.0.1:6811/add?uri=';const s=String(getSelection()).trim();const a=[...document.querySelectorAll('a[href^="magnet:"],a[href$=".torrent"]')].map(e=>e.href);const u=/^(magnet:|https?:|ftp:)/i.test(s)?s:prompt('Link to add to dman:',a[0]||location.href);if(u)open(d+encodeURIComponent(u),'_blank','noopener')})()
```

### Userscript

For Violentmonkey, Tampermonkey or Greasemonkey. It adds a small "dman" link after every
magnet and `.torrent` link; clicking it opens dman's add page with that link.

```js
// ==UserScript==
// @name         Send to dman
// @description  Adds a "dman" link next to magnet and .torrent links; it opens dman's add page.
// @match        *://*/*
// @grant        none
// ==/UserScript==
(() => {
  const dman = 'http://127.0.0.1:6811/add?uri=';
  const mark = (a) => {
    if (a.dataset.dman) return;
    a.dataset.dman = '1';
    const b = document.createElement('a');
    b.textContent = ' [dman]';
    b.href = dman + encodeURIComponent(a.href);
    b.target = '_blank';
    b.rel = 'noopener noreferrer';
    a.after(b);
  };
  const scan = () =>
    document.querySelectorAll('a[href^="magnet:"], a[href$=".torrent"]').forEach(mark);
  scan();
  new MutationObserver(scan).observe(document.body, { childList: true, subtree: true });
})();
```

### With a login

When the web UI requires a login (`require_login`, or a network address), its session
cookie is `SameSite=Strict`, so the browser does not send it on a page opened from another
site: you land on the login page and the link is not carried over. Use the handler
([above](#handler-for-magnet-links-and-torrent-files)) or the [clipboard](#clipboard) in that case, or open the add page yourself and paste.

## Clipboard

```sh
dman add --clipboard     # add the link(s) in the clipboard
dman add                 # in a terminal without arguments: offers the clipboard's link
```

| Platform | Clipboard tool |
| --- | --- |
| Wayland | `wl-paste` |
| X11 | `xclip` or `xsel` |
| macOS | `pbpaste` |
| Windows | PowerShell `Get-Clipboard` |

Install one of them if dman says it cannot read the clipboard. Only text that consists of URLs or magnet links (one per line) is taken.

## Duplicates

All of these paths go through the daemon's add, which refuses a link that is already in
your downloads (same URL, or same torrent info hash) and names the existing job. In the
CLI this is exit code 9 with the hint `use --force to add anyway`; the TUI and the web UI
show the same message. A duplicate torrent receives the trackers of the new link instead.
