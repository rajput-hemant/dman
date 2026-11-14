---
name: release
description: 'Runs a full dman release end to end: version bump, tag, CI publish dispatch, crates.io, Scoop, winget, Homebrew check and a status table.'
---

# dman release runbook

Run from the repository root. `docs/releasing.md` and the workflow files win over this skill.

## 0. One-time setup

See the setup section of `docs/releasing.md`: `cargo login` with a crates.io token, a classic GitHub token with `public_repo`, komac installed, and the `HOMEBREW_TAP_TOKEN` repo secret.

## 1. Preconditions

`git status --short` empty, branch `main`, `gh run list --limit 5` green. In `Cargo.toml`, `[workspace.package].version` and every `dman-*` entry under `[workspace.dependencies]` must carry the same version. Crates inherit `version.workspace = true` and packaging templates keep `__VERSION__` for `fill.sh`, so edit neither.

## 2. Bump and verify

Set those versions to `X.Y.Z`, then:

```bash
./scripts/check.sh          # fmt, clippy -D warnings, tests; also updates Cargo.lock
git add Cargo.toml Cargo.lock
git commit -m "chore: release vX.Y.Z"
```

## 3. Tag and dry run

`git tag vX.Y.Z && git push origin main vX.Y.Z`

Expect the Release workflow to build 5 archives plus installers and publish nothing. If the workflow changed after the tag existed, move it first: `git tag -f vX.Y.Z && git push --force origin vX.Y.Z`.

## 4. Publish (ask the user first)

The dispatch creates a public release and pushes the tap formula. Get an explicit yes, then:

```bash
gh workflow run release.yml --ref vX.Y.Z -f publish=true
id=$(gh run list --workflow Release --limit 1 --json databaseId -q '.[0].databaseId')
gh run watch "$id"
```

Pass the id: a bare `gh run watch` can attach to the dry-run run, which has no `publish-homebrew-formula` job. Expect green, then `gh release view vX.Y.Z` must list 5 archives each with `.sha256`, `dman-installer.sh`, `dman-installer.ps1`, `dman.rb`, `sha256.sum` and `source.tar.gz`.

## 5. crates.io (12 crates, ask the user first)

crates.io allows a burst of 5 new crates, then 1 per 10 minutes: about 70 minutes, and a published version cannot be retracted. Never use `cargo publish --workspace`; after a partial run it retries everything and fails on the first crate that already exists. After the user agrees:

```bash
C="dman-proto dman-torrent dman-ytdlp dman-search dman-aria2 dman-store dman-client dman-config dman-web dman-tui dman-daemon dman"
for c in $C; do cargo publish -p "$c" || { echo "failed: $c"; exit 1; }; [ "$c" = dman ] || sleep 600; done
```

Every `dman-*` dependency comes before its dependent. On HTTP 429, wait 10 minutes and rerun from the failing crate. Then prove it before the status table:

```bash
for c in $C; do cargo info -q "$c" >/dev/null 2>&1 && echo "ok $c" || echo "MISSING $c"; done
```

## 6. Scoop and winget

```bash
packaging/fill.sh X.Y.Z
[ -d ../scoop-bucket ] || git clone https://github.com/rajput-hemant/scoop-bucket ../scoop-bucket
cp target/packaging/X.Y.Z/scoop/dman.json ../scoop-bucket/bucket/dman.json
git -C ../scoop-bucket add bucket/dman.json
git -C ../scoop-bucket commit -m "dman: Update to version X.Y.Z"
git -C ../scoop-bucket push
```

winget uses komac (`gh repo fork microsoft/winget-pkgs --clone=false` once):

```bash
GITHUB_TOKEN=$(gh auth token) komac submit target/packaging/X.Y.Z/winget -y
```

Use `komac submit` for every release until the winget-pkgs pull request is merged; only then does `komac update rajput-hemant.dman --version X.Y.Z --urls <zip url> --submit` work. `komac submit --dry-run` validates nothing, so compare `InstallerSha256` with the release zip first.

## 7. Homebrew

`gh api 'repos/rajput-hemant/homebrew-tap/commits?path=Formula/dman.rb&per_page=1' --jq '.[0].commit.message'` prints `dman X.Y.Z`.

## 8. Status table

Fill each row only from the proof command, never by assumption.

| Channel | Proof |
| --- | --- |
| GitHub release | `gh release view` asset list |
| Homebrew | tap commit message above |
| crates.io | the `cargo info` loop, all 12 ok |
| Scoop | bucket commit on main |
| winget | pull request number in microsoft/winget-pkgs |
| AUR | blocked: no AUR account, nothing to do |
