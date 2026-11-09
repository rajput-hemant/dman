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

- Rust only. Workspace crates live in `crates/`. One shipped binary: `dman`.
- Keep it small: add a dependency only if it is listed in `PROGRESS.md` or clearly
  justified; prefer minimal feature flags. Permissive licenses only (MIT/Apache/BSD/ISC/Zlib).
- Every change must pass: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`.
- Commits: one per coherent unit of work (a crate, a feature, a fix) — not per file,
  not per milestone. Imperative subject ≤ 72 chars, body explains why.
- Planning notes and scratch files go in `.plan/`, `spikes/` or `notes/` (git-ignored).
  Only `README.md`, `CLAUDE.md` and `PROGRESS.md` are committed markdown at the root.
- Report progress to the user in 10 % steps of overall completion (weights in `PROGRESS.md`).
- Delegation: small mechanical work → haiku; normal feature work → sonnet;
  architecture-critical or tricky concurrency work → opus. Review agent output before committing.
- Safety rules from the spec are non-negotiable: never delete files unless explicitly
  requested; secrets never logged; destructive actions confirmed.
