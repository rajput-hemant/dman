# Releasing dman

Pushing a tag `vX.Y.Z` runs `.github/workflows/release.yml` (cargo-dist). It builds
the archives for Linux (musl, x86_64 and aarch64), macOS (x86_64 and aarch64) and
Windows (x86_64), the shell and PowerShell installers, publishes the GitHub release
and pushes the Homebrew formula to the tap. crates.io, Scoop, winget and the AUR are
updated by hand from the published release, as below.

## One-time setup

- **Homebrew**: create the public repo `rajput-hemant/homebrew-tap`. Create a
  fine-grained token with *Contents: read and write* on that repo only, and add it to
  `rajput-hemant/dman` as the Actions secret `HOMEBREW_TAP_TOKEN`
  (Settings → Secrets and variables → Actions).
- **crates.io**: sign in at crates.io with GitHub, verify your email, create an API
  token with the `publish-new` and `publish-update` scopes, then run `cargo login`.
- **Scoop**: create the public repo `rajput-hemant/scoop-bucket` with a `bucket/`
  directory.
- **winget** (on Windows): `winget install Microsoft.WingetCreate`. Create a classic
  GitHub token with the `public_repo` scope; wingetcreate forks
  `microsoft/winget-pkgs` and opens the pull request with it.
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

5. **Wait for release.yml** (`gh run watch`, or the Actions tab). It must finish
   green, including the `publish-homebrew` job.
6. **crates.io**: `cargo publish --workspace` publishes all crates in dependency
   order. crates.io limits how fast *new* crates can be created; if it stops with
   HTTP 429, wait the time it names and publish the rest with `cargo publish -p <name>`
   in this order:

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

9. **winget** (Windows, PowerShell, in the repo; run step 7 there with Git Bash's `sh`).
   First release:

   ```powershell
   winget validate --manifest .\target\packaging\X.Y.Z\winget
   wingetcreate submit --token $env:WINGET_TOKEN .\target\packaging\X.Y.Z\winget
   ```

   Later releases (wingetcreate downloads the zip and computes the hash itself):

   ```powershell
   wingetcreate update rajput-hemant.dman --version X.Y.Z --token $env:WINGET_TOKEN --submit `
     --urls https://github.com/rajput-hemant/dman/releases/download/vX.Y.Z/dman-x86_64-pc-windows-msvc.zip
   ```

   The package goes live when the winget-pkgs pull request is merged.

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
