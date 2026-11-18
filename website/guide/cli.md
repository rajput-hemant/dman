# CLI

Every command is scriptable. Output is a table for people; with the global `--json` flag every
command prints one JSON document on stdout, and errors become
`{"error": {"code": ..., "message": ...}}` on stderr with a `hint` when there is a next step.
Questions are asked only in a terminal; scripts pass `--yes` to destructive commands.

## Global options

| Flag | Effect |
| --- | --- |
| `--profile NAME` | Select a daemon profile (`DMAN_PROFILE` also works) |
| `--json` | Machine-readable output, also for errors |
| `-v` | More logging (`-v` info, `-vv` debug, `-vvv` trace) |

## Commands

Run `dman --help` for the full list and `dman <command> --help` for flags and examples.

| Task | Commands |
| --- | --- |
| Add | `dman add`, `dman preview` (files of a magnet), `dman probe` (video formats) |
| Inspect | `dman list`, `dman get ID`, `dman wait ID`, `dman stats` |
| Control | `dman pause`, `dman resume`, `dman remove`, `dman restore`, `dman purge` |
| Change | `dman set`, `dman rename`, `dman reorder`, `dman cleanup` |
| Limits and queue | `dman limit`, `dman max-concurrent`, `dman schedule` |
| Search | `dman search`, `dman providers`, `dman trackers status` (see [Search](./search)) |
| Video | `dman video prefs` (quality picker defaults) |
| Daemon | `dman daemon start\|stop\|restart\|status\|engine-restart\|gen-config` |
| Web UI and browser | `dman serve` (see [Web UI](./web-ui)), `dman integrate`, `dman handle` (see [Browser](./browser)) |
| Maintenance | `dman config` (see [Configuration](./configuration)), `dman update`, `dman completions install` |

## Adding downloads

```bash
dman add https://example.org/big.iso
dman add magnet:?xt=urn:btih:... --select-files 1,3-5
dman add ubuntu.torrent --dir ~/iso --paused
dman add -i urls.txt --max-download-limit 2MiB
dman add --mirror https://a.example/f.zip https://b.example/f.zip
dman add --clipboard
```

Video URLs ask for a quality in a terminal; pick it up front with flags instead:

```bash
dman add https://youtu.be/ID --quality 1080p --container mkv --subs en
dman probe https://youtu.be/ID --plan   # see what add would do
```

A URL or torrent already in the downloads (same URL or info hash) is refused with exit code 9
and a hint to use `--force`; a known torrent gets the new link's trackers instead.

### Google Drive folders

`dman add` also takes a public or link-shared Google Drive folder link, on its own:

```bash
dman add https://drive.google.com/drive/folders/FOLDER_ID
```

Every file in the folder becomes a normal download, subfolders included, and the folder tree
is recreated under the target directory in a folder named after the Drive folder. Files
already in the list are skipped, so adding the folder again picks up only what is new
(`--force` adds them anyway). If one file cannot be added, the downloads created for that
folder are removed again. Private folders and empty folders are refused; share the folder as
"anyone with the link". Links to single files (`/file/d/...`) are not folder links.

## Changing downloads

```bash
dman set 12 --max-download-limit 1MiB       # 0 = unlimited
dman set 12 --select 1,3-5                  # choose the files of a torrent
dman set 12 --dir ~/iso                     # only while waiting or paused
dman set 12 --header 'Cookie: a=b' --connections 4
dman set 12 --position 0                    # move to the front of the queue
dman rename 12 ubuntu.iso
dman remove 12                              # asks first; dman restore 12 undoes it
dman remove 12 --delete-files               # also deletes the files; cannot be undone
```

Options that need a stopped transfer (folder, files, headers, connections) are refused while
the download runs: pause it first.

## Waiting and exit codes

`dman add --wait` and `dman wait ID` show progress and exit when the download finishes. A
torrent counts as finished once it completes; seeding does not delay the exit. `wait` exits 0
when everything completed, 1 when every download failed, 8 when some did and 124 when
`--timeout` expired (the downloads keep running).

| Code | Meaning |
| --- | --- |
| 0 | success |
| 1 | general error |
| 2 | usage error or invalid input |
| 3 | the daemon cannot be reached |
| 4 | unauthorized (wrong or missing secret) |
| 5 | not found (unknown job ID, provider, ...) |
| 6 | refused (needs `--yes`, disk full, not allowed here) |
| 7 | engine unavailable (aria2 or yt-dlp missing) |
| 8 | partial failure (some items of a batch failed) |
| 9 | already in the downloads (use `--force`) |
| 124 | timed out (`--timeout`) |
| 130 | interrupted (Ctrl-C) |

## Tab completion

```bash
dman completions install                  # install for the shell in $SHELL
dman completions install --shell fish     # or choose the shell
dman completions bash > dman.bash         # print a script instead (for packagers)
```

Completion offers job IDs with their names, providers, profiles and options. Bash, zsh and
fish are supported.
