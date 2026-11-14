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

| Task | Commands |
| --- | --- |
| Add and wait | `dman add URL`, `dman add --clipboard`, `dman add --wait` |
| Inspect | `dman list`, `dman get ID`, `dman stats`, `dman probe URL` |
| Control | `dman pause`, `dman resume`, `dman remove`, `dman limit`, `dman max-concurrent` |
| Search | `dman search`, `dman providers`, `dman trackers status` |
| Daemon | `dman daemon start\|stop\|restart\|status\|engine-restart\|gen-config` |
| Web UI | `dman serve`, `dman serve --stop` |
| Maintenance | `dman config`, `dman update`, `dman completions install` |

Run `dman --help` for the full list and `dman <command> --help` for flags and examples.

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

## Changing downloads

```bash
dman set 12 --max-download-limit 1MiB       # 0 = unlimited
dman set 12 --select 1,3-5                  # choose the files of a torrent
dman set 12 --dir ~/iso                     # only while waiting or paused
dman set 12 --header 'Cookie: a=b' --connections 4
dman set 12 --position 0                    # move to the front of the queue
dman rename 12 ubuntu.iso
dman remove 12                              # asks first; dman restore 12 undoes it
```

Options that need a stopped transfer (folder, files, headers, connections) are refused while
the download runs: pause it first.

## Waiting and exit codes

`dman add --wait` and `dman wait ID` show progress and exit when the download finishes. A
torrent counts as finished once it completes; seeding does not delay the exit.

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
| 124 | timed out (`--timeout`) |

## Tab completion

```bash
dman completions install                  # install for the shell in $SHELL
dman completions install --shell fish     # or choose the shell
```

Completion offers job IDs with their names, providers, profiles and options. Bash, zsh and
fish are supported.
