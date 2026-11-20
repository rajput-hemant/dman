---
name: release
description: 'Runs a full dman release end to end: version bump, tag, CI publish dispatch, crates.io, Scoop, winget, Homebrew check and a status table.'
---

# dman release runbook

Run from the repository root. `docs/releasing.md` and the workflow files win over this skill.

## Steps

Follow `docs/releasing.md` (setup, bump, PR, tag, dry run, publish dispatch, crates.io, Scoop, winget, AUR, verify); it holds every command. This skill adds only what an agent must not skip:

- Run from the repository root with a clean `git status --short` and green `gh run list --limit 5`. Edit only the `[workspace.package]` version and the `dman-*` entries under `[workspace.dependencies]` in `Cargo.toml`; crates inherit the version and `packaging/` templates keep `__VERSION__` for `fill.sh`.
- `main` takes pull requests only: the version bump goes through a PR, and the tag is made on the merged commit.
- Ask the user for an explicit yes before the publish dispatch (it creates a public release and pushes the tap formula) and before crates.io (about 70 minutes, a published version cannot be retracted). Never tag or publish unasked.
- Watch the publish run by its id, not a bare `gh run watch`.
- On crates.io HTTP 429, wait 10 minutes and rerun from the failing crate. Prove the result before reporting:

```bash
C="dman-proto dman-torrent dman-ytdlp dman-search dman-aria2 dman-store dman-client dman-config dman-web dman-tui dman-daemon dman"
for c in $C; do cargo info -q "$c" >/dev/null 2>&1 && echo "ok $c" || echo "MISSING $c"; done
```

- winget: use `komac submit` until the winget-pkgs pull request is merged; `komac submit --dry-run` validates nothing, so compare `InstallerSha256` with the release zip first.
- Homebrew proof: `gh api 'repos/rajput-hemant/homebrew-tap/commits?path=Formula/dman.rb&per_page=1' --jq '.[0].commit.message'` prints `dman X.Y.Z`.

## Status table

Fill each row only from the proof command, never by assumption.

| Channel | Proof |
| --- | --- |
| GitHub release | `gh release view` asset list |
| Homebrew | tap commit message above |
| crates.io | the `cargo info` loop, all 12 ok |
| Scoop | bucket commit on main |
| winget | pull request number in microsoft/winget-pkgs |
| AUR | blocked: no AUR account, nothing to do |
