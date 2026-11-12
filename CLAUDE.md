# dman — instructions for Claude sessions

dman is a terminal-first download manager in Rust. aria2c is the download engine,
yt-dlp (+ffmpeg) is an optional video engine. A dman daemon owns both engines and all
state; the TUI (ratatui), web UI (server HTML + htmx) and CLI are clients of one
JSON-RPC-over-WebSocket API.

## Resuming work (do this first, every session)

1. Read `PROGRESS.md`. It is the single source of truth for what is done, what is
   next, and every confirmed design decision.
2. If `.plan/implementation-plan.md` exists locally, it holds the full plan; if not
   (fresh clone on another device), `PROGRESS.md` contains everything required.
3. Continue with the first unchecked task under "Next up". Do not re-ask decisions
   listed as confirmed in `PROGRESS.md`; ask the user only about genuinely new choices.
4. After finishing a task: tick it in `PROGRESS.md`, update "Current state", and
   commit code and tracker together.

## Rules

- Rust only. Workspace crates live in `crates/` (`dman` binary, `dman-proto`, `dman-config`,
  `dman-store`, `dman-aria2`, `dman-ytdlp`, `dman-torrent`, `dman-search`, `dman-daemon`,
  `dman-client`, `dman-tui`, `dman-web`). One shipped binary: `dman`.
- Keep it small: add a dependency only if it is listed in `PROGRESS.md` or clearly
  justified; prefer minimal feature flags. Permissive licenses only (MIT/Apache/BSD/ISC/Zlib).
- Every change must pass (`scripts/check.sh` runs the first five): `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`,
  `cargo clippy -p dman --no-default-features --all-targets -- -D warnings`,
  `cargo test -p dman --no-default-features`. Platform code also needs
  `cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu -- -D warnings`;
  daemon behaviour changes also `DMAN_TEST_ARIA2=1 DMAN_TEST_YTDLP=1 cargo test -p dman-daemon -- --include-ignored --test-threads=1`.
- Releases are built by CI (cargo-dist, `dist-workspace.toml`). Do not tag or publish
  without being asked.
- Commits: one per coherent unit of work (a crate, a feature, a fix) — not per file,
  not per milestone. Conventional Commits format (`feat:`, `fix:`, `refactor:`,
  `docs:`, `test:`, `chore:`, `perf:`, `ci:`; optional scope), imperative subject
  ≤ 72 chars, body explains why. Author: `rajput-hemant <rajput.hemant2001@gmail.com>`.
  No `Co-Authored-By` or other attribution trailers.
- Code comments must not reference planning IDs (decision codes like D22, Q3, U1
  or milestone numbers); explain the reason in plain words instead.
- Planning notes and scratch files go in `.plan/`, `spikes/` or `notes/` (git-ignored).
  Only `README.md`, `CLAUDE.md` and `PROGRESS.md` are committed markdown at the root.
- Report progress to the user in 10 % steps of overall completion (weights in `PROGRESS.md`).
- Delegation: small mechanical work → haiku; normal feature work → sonnet;
  architecture-critical or tricky concurrency work → opus. Review agent output before committing.
- Safety rules from the spec are non-negotiable: never delete files unless explicitly
  requested; secrets never logged; destructive actions confirmed.
