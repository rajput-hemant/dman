# Releasing dman

**TL;DR:** bump the version, run the checks, then tag `vX.Y.Z` and push the tag. That runs
`.github/workflows/release.yml` (cargo-dist) as a dry run: it builds the archives and
installers but publishes nothing. Publishing is a manual dispatch on that tag with
`publish=true`, which creates the GitHub release and pushes the Homebrew formula.
crates.io, Scoop, winget and the AUR are updated by hand from the published release.

## Contents

- [What CI publishes](#what-ci-publishes)
- [One-time setup](#one-time-setup)
- [Every release](#every-release)
- [Verify](#verify)
- [Building for all platforms locally](#building-for-all-platforms-locally)

## What CI publishes

Nothing on a tag push: that is a dry run. The release itself is the manual dispatch from
step 5.

| Output | Channel |
| --- | --- |
| Archives: Linux (musl, x86_64 and aarch64), macOS (x86_64 and aarch64), Windows (x86_64) | GitHub release |
| Shell and PowerShell installers | GitHub release |
| Homebrew formula | Tap `rajput-hemant/homebrew-tap` (by the dispatch) |
| crates.io, Scoop, winget, AUR | By hand, steps 6 to 10 below |

## One-time setup

- **Homebrew**: create the public repo `rajput-hemant/homebrew-tap`. Create a
  fine-grained token with *Contents: read and write* on that repo only, and add it to
  `rajput-hemant/dman` as the Actions secret `HOMEBREW_TAP_TOKEN`
  (Settings → Secrets and variables → Actions).
- **crates.io**: sign in at crates.io with GitHub, verify your email, create an API
  token with the `publish-new` and `publish-update` scopes, then run `cargo login`.
- **Scoop**: create the public repo `rajput-hemant/scoop-bucket` with a `bucket/`
  directory.
- **winget** (any OS): install
  [komac](https://github.com/russellbanks/Komac), then create a classic GitHub token
  with the `public_repo` scope. Komac forks `microsoft/winget-pkgs` and opens the
  pull request with it.
- **AUR**: create an account on aur.archlinux.org and add an SSH public key under
  *My Account*. Add to `~/.ssh/config`:

  ```
  Host aur.archlinux.org
    User aur
    IdentityFile ~/.ssh/aur
  ```

## Every release

Run the commands from the repository root unless a step says otherwise.

1. **Bump the version** in `Cargo.toml`: `version` under `[workspace.package]` and the
   `version = "…"` of every `dman-*` entry under `[workspace.dependencies]` (all
   equal). Then run `cargo check` to update `Cargo.lock`.
2. **Changelog**: if `CHANGELOG.md` exists, add the release; cargo-dist uses its
   section as the release notes.
3. **Check**: `cargo fmt --all --check`,
   `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
4. **Tag and push**:

   ```bash
   git commit -am "chore: release vX.Y.Z"
   git tag vX.Y.Z
   git push origin main vX.Y.Z
   ```

5. **Dry run**: the tag push runs release.yml, which builds and uploads artifacts but
   publishes nothing. Wait for it to finish green (the `build-local` jobs are the
   build check), then publish:

   ```bash
   gh workflow run release.yml --ref vX.Y.Z -f publish=true
   gh run watch
   ```

   The dispatch must run on the tag, and it must finish green including the
   `publish-homebrew-formula` job. `publish=false` (or a plain tag push) never releases.
   The run uses the workflow file *at that tag*, so move the tag if the workflow changed
   after it was created:

   ```bash
   git tag -f vX.Y.Z && git push --force origin vX.Y.Z
   ```
6. **crates.io**: crates.io allows a burst of 5 new crates, then 1 per 10 minutes,
   so the 12 crates take about 70 minutes. Do not use `cargo publish --workspace`:
   after a partial run it retries the whole set and fails on the first crate that
   already exists. Publish one at a time with `cargo publish -p <name>`, in this
   order (every crate's `dman-*` dependencies come before it):

   `dman-proto`, `dman-torrent`, `dman-ytdlp`, `dman-search`, `dman-aria2`,
   `dman-store`, `dman-client`, `dman-config`, `dman-web`, `dman-tui`,
   `dman-daemon`, `dman`

7. **Fill the templates** with the release's checksums:

   ```bash
   packaging/fill.sh X.Y.Z        # writes target/packaging/X.Y.Z/{scoop,winget,aur}
   ```

8. **Scoop**:

   ```bash
   cp target/packaging/X.Y.Z/scoop/dman.json ../scoop-bucket/bucket/dman.json
   cd ../scoop-bucket && git add bucket/dman.json
   git commit -m "dman: Update to version X.Y.Z" && git push
   ```

9. **winget** (any OS, with [komac](https://github.com/russellbanks/Komac); a
   GitHub token with `public_repo` is enough). The first release needs a fork of
   `microsoft/winget-pkgs`:

   ```bash
   gh repo fork microsoft/winget-pkgs --clone=false
   GITHUB_TOKEN=$(gh auth token) komac submit target/packaging/X.Y.Z/winget -y
   ```

   Later releases, once the package exists:

   ```bash
   GITHUB_TOKEN=$(gh auth token) komac update rajput-hemant.dman --version X.Y.Z \
     --urls https://github.com/rajput-hemant/dman/releases/download/vX.Y.Z/dman-x86_64-pc-windows-msvc.zip --submit
   ```

   `komac submit --dry-run` validates nothing, so compare `InstallerSha256` with
   the release zip yourself before submitting. The package goes live when the
   winget-pkgs pull request is merged.

10. **AUR** (on Arch, as a normal user; makepkg refuses to run as root):

    ```bash
    git clone ssh://aur@aur.archlinux.org/dman-bin.git ../dman-bin   # first time only
    cp target/packaging/X.Y.Z/aur/PKGBUILD ../dman-bin/
    cd ../dman-bin
    makepkg --printsrcinfo > .SRCINFO
    makepkg -si                       # test build and install
    git add PKGBUILD .SRCINFO && git commit -m "Update to X.Y.Z" && git push
    ```

## Verify

- [ ] The release has five archives, each with a `.sha256`, plus `dman-installer.sh`,
      `dman-installer.ps1`, `dman.rb` and `sha256.sum`.
- [ ] Shell and PowerShell installers: `dman --version` shows X.Y.Z and
      `dman update --check` says it is up to date.
- [ ] `brew install rajput-hemant/tap/dman` installs aria2 too.
- [ ] `scoop install rajput-hemant/dman`, `winget install rajput-hemant.dman` (after
      the merge) and `yay -S dman-bin` install X.Y.Z.
- [ ] crates.io shows all twelve crates at X.Y.Z, and `cargo install --locked dman`
      builds.
- [ ] On a package-manager install, `dman update` prints that manager's command and
      changes nothing.

## Building for all platforms locally

Local builds are for testing. Official release artifacts come only from the CI tag workflow
(cargo-dist). All five release targets were built this way on macOS arm64. Apple targets are
built on macOS.

One-time setup (macOS):

```bash
brew install zig llvm
cargo install --locked cargo-zigbuild cargo-xwin
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl \
  x86_64-apple-darwin aarch64-apple-darwin x86_64-pc-windows-msvc
```

| Target | Command | Output |
| --- | --- | --- |
| `x86_64-unknown-linux-musl` | `cargo zigbuild --release -p dman --target x86_64-unknown-linux-musl` | `target/x86_64-unknown-linux-musl/release/dman` |
| `aarch64-unknown-linux-musl` | `cargo zigbuild --release -p dman --target aarch64-unknown-linux-musl` | `target/aarch64-unknown-linux-musl/release/dman` |
| `x86_64-apple-darwin` | `cargo zigbuild --release -p dman --target x86_64-apple-darwin` | `target/x86_64-apple-darwin/release/dman` |
| `aarch64-apple-darwin` | `cargo zigbuild --release -p dman --target aarch64-apple-darwin` | `target/aarch64-apple-darwin/release/dman` |
| `x86_64-pc-windows-msvc` | `cargo xwin build --release -p dman --target x86_64-pc-windows-msvc` | `target/x86_64-pc-windows-msvc/release/dman.exe` |
