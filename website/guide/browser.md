# Browser

There are three ways to get a magnet link, a `.torrent` file or a download URL from the
browser into dman. None of them weakens the web UI's protection against other websites.

| Way | Needs | Confirms before adding |
| --- | --- | --- |
| Handler of magnet links and `.torrent` files | `dman integrate install` (Linux, Windows) | no, adds at once and shows a notification |
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
the result. A link that is already in your downloads is not added again; for a torrent its
new trackers are added to the existing job.

| Platform | Mechanism |
| --- | --- |
| Linux | A `.desktop` file plus `xdg-mime default` |
| Windows | Registry keys under `HKEY_CURRENT_USER\Software\Classes` |
| macOS | Manual: `install` prints the steps (a URL scheme needs an application bundle) |

## Bookmarklet and userscript (web UI)

The web UI refuses state-changing requests from other websites. A bookmarklet can still
**open the add page with the link filled in**: `GET /add?uri=...` only fills the form, and
you confirm with **Add**. Nothing is added without that click.

This works as is for the default, login-free web UI on `http://127.0.0.1:6811`; adjust the
address if you changed `[web]`.

Bookmarklet: create a bookmark with this URL. It takes the selected text if it is a link,
otherwise asks, suggesting the first magnet or `.torrent` link of the page.

```js
javascript:(()=>{const d='http://127.0.0.1:6811/add?uri=';const s=String(getSelection()).trim();const a=[...document.querySelectorAll('a[href^="magnet:"],a[href$=".torrent"]')].map(e=>e.href);const u=/^(magnet:|https?:|ftp:)/i.test(s)?s:prompt('Link to add to dman:',a[0]||location.href);if(u)open(d+encodeURIComponent(u),'_blank','noopener')})()
```

Userscript (Violentmonkey, Tampermonkey or Greasemonkey): adds a "dman" link after every
magnet and `.torrent` link.

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

When the web UI requires a login (`require_login`, or a network address), its session cookie
is `SameSite=Strict`, so the link is not carried over to the login page: use the handler or
the clipboard instead.

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

Install one of them if dman says it cannot read the clipboard. Only text that consists of
URLs or magnet links (one per line) is taken.

## Duplicates

All of these paths go through the daemon's add, which refuses a link that is already in your
downloads (same URL, or same torrent info hash) and names the existing job. In the CLI this
is exit code 9 with the hint `use --force to add anyway`; a duplicate torrent receives the
trackers of the new link instead.
