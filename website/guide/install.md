# Install

## Package managers

Homebrew, Scoop, winget and the AUR package install aria2 together with dman. With the other
methods, install `aria2c` yourself; `yt-dlp` and `ffmpeg` are optional everywhere.

| Method | Platform | Command |
| --- | --- | --- |
| Shell installer | Linux, macOS | `curl --proto '=https' --tlsv1.2 -LsSf https://github.com/rajput-hemant/dman/releases/latest/download/dman-installer.sh \| sh` |
| PowerShell installer | Windows | `powershell -ExecutionPolicy Bypass -c "irm https://github.com/rajput-hemant/dman/releases/latest/download/dman-installer.ps1 \| iex"` |
| Homebrew | macOS, Linux | `brew install rajput-hemant/tap/dman` |
| Scoop | Windows | `scoop bucket add rajput-hemant https://github.com/rajput-hemant/scoop-bucket` then `scoop install rajput-hemant/dman` |
| winget | Windows | `winget install rajput-hemant.dman` |
| AUR | Arch Linux | `yay -S dman-bin` |
| crates.io | any (builds from source) | `cargo install --locked dman` |

## Prebuilt archives

Archives for five targets are on the
[releases page](https://github.com/rajput-hemant/dman/releases):

| Platform | Target |
| --- | --- |
| Linux x86_64 | `x86_64-unknown-linux-musl` |
| Linux aarch64 | `aarch64-unknown-linux-musl` |
| macOS Intel | `x86_64-apple-darwin` |
| macOS Apple silicon | `aarch64-apple-darwin` |
| Windows x86_64 | `x86_64-pc-windows-msvc` |

## Shell completions

`dman completions install` sets up tab completion for bash, zsh or fish; see
[CLI](./cli#tab-completion).

## Updating

Installs made with an installer update themselves with `dman update`; for the other methods it
prints the package manager's command. `dman update --check` shows the installed and the latest
version.

## Building from source

Rust 1.97 or newer is required; `rust-toolchain.toml` follows the stable channel and `rustup` installs it on first use.

```bash
cargo build --release -p dman
```

### Build features

Slimmer source builds are possible with `--no-default-features --features web,search`.

| Feature | Provides |
| --- | --- |
| `web` | `dman serve` and the browser UI |
| `search` | Torrent search, providers and the public tracker list |
| `video` | The yt-dlp video engine |
| `notify` | Desktop notifications (daemon and TUI) |

All four are on by default.
