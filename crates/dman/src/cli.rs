//! Command-line definition (clap derive).

use clap::{ArgAction, Args, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use dman_proto::{JobId, JobState};

use crate::completion as comp;

const EXAMPLES: &str = "\
Examples:
  dman                                    open the interactive UI (starts the daemon if needed)
  dman daemon start                       start the background daemon
  dman add https://example.org/big.iso    download a file
  dman search ubuntu --add 1              search the enabled providers and add result 1
  dman list --state active,paused         show running and paused downloads
  dman serve                              open the browser UI on http://127.0.0.1:6811
  dman completions install                enable tab completion for your shell";

/// Heading of the options every command accepts.
const GLOBAL: &str = "Global options";

/// What `dman --help` says beyond the one-line summary, including the exit codes.
pub const LONG_ABOUT: &str = "\
dman: a terminal-first download manager.

A background daemon owns aria2 (and yt-dlp for video) and all state. This CLI, the
interactive UI (run `dman` without arguments) and the browser UI (`dman serve`)
are clients of it.

Tab completion (job IDs with their names, providers, profiles, options) is
installed with `dman completions install`; `dman completions <shell>` prints the
script instead.

Output is a table for people. With `--json` every command prints one JSON document
on stdout, and errors become {\"error\": {\"code\": ..., \"message\": ...}} on stderr
(plus a \"hint\" when there is a next step to suggest).
Questions are asked only in a terminal; scripts pass `--yes` to destructive commands.

Exit codes:
  0    success
  1    general error
  2    usage error or invalid input
  3    the daemon cannot be reached (is it running?)
  4    unauthorized (wrong or missing secret)
  5    not found (unknown job ID, provider, ...)
  6    refused (needs --yes, disk full, not allowed here)
  7    engine unavailable (aria2 or yt-dlp missing or not running)
  8    partial failure (some items of a batch failed)
  9    already in the downloads (add: same URL or torrent; use --force)
  124  timed out (`dman wait --timeout`, `dman add --wait --timeout`)
  130  interrupted (Ctrl-C)";

/// dman: a terminal-first download manager.
#[derive(Parser, Debug)]
#[command(name = "dman", version, propagate_version = true, long_about = LONG_ABOUT, after_help = EXAMPLES)]
pub struct Cli {
    /// Daemon profile to use.
    #[arg(long, global = true, env = "DMAN_PROFILE", value_name = "NAME", add = comp::profiles(), help_heading = GLOBAL)]
    pub profile: Option<String>,
    /// Machine-readable output (JSON), also for errors.
    #[arg(long, global = true, help_heading = GLOBAL)]
    pub json: bool,
    /// Increase log verbosity (-v info, -vv debug, -vvv trace).
    #[arg(short, long, global = true, action = ArgAction::Count, help_heading = GLOBAL)]
    pub verbose: u8,
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// The command definition used for parsing and completion. `pause` and `resume` share
/// [`TargetArgs`]; resume completes paused jobs instead.
pub fn command() -> clap::Command {
    Cli::command().mut_subcommand("resume", |c| {
        c.mut_arg("ids", |a| a.add(comp::job_ids_paused()))
    })
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Manage the background daemon.
    #[command(after_help = "\
Examples:
  dman daemon start
  dman daemon status
  dman daemon restart")]
    #[command(display_order = 25)]
    Daemon {
        #[command(subcommand)]
        cmd: DaemonCmd,
    },
    /// Add downloads (URLs, magnets, .torrent/.metalink files).
    #[command(after_help = "\
Examples:
  dman add https://example.org/big.iso
  dman add magnet:?xt=urn:btih:... --select-files 1,3-5
  dman add ubuntu.torrent --dir ~/iso --paused
  dman add -i urls.txt --max-download-limit 2MiB
  dman add --mirror https://a.example/f.zip https://b.example/f.zip
  dman add https://youtu.be/ID                     choose the quality (asks in a terminal)
  dman add https://youtu.be/ID --quality 1080p --container mkv --subs en
  dman add --clipboard                             add the link in the clipboard
  dman add https://example.org/ubuntu.torrent      fetched by the daemon, added as a torrent
  dman add magnet:?xt=urn:btih:... --force         add even if it is already in the list
  dman add https://example.org/big.iso --wait      show progress, exit when it is done

Adding a URL or torrent that is already in the list (same URL or info hash) is
refused with exit code 9 and the existing download's ID; a known torrent gets the
new link's trackers instead.")]
    #[command(display_order = 1)]
    Add(Box<AddArgs>),
    /// Change options of existing downloads (limits, folder, files, seeding, headers).
    #[command(after_help = "\
Examples:
  dman set 12 --max-download-limit 1MiB       limit one download (0 = unlimited)
  dman set 12 13 --max-upload-limit 200K
  dman set 12 --select 1,3-5                  choose the files of a torrent
  dman set 12 --seed-ratio 2 --seed-time 120
  dman set 12 --dir ~/iso                     only while waiting or paused
  dman set 12 --header 'Cookie: a=b' --connections 4
  dman set 12 --position 0                    move to the front of the queue
  dman set 12 --seed-ratio 1 --json           print the updated download as JSON

Options that need a stopped transfer (folder, files, headers, connections) are
refused while the download runs: pause it first (`dman pause 12`).")]
    #[command(display_order = 7)]
    Set(Box<SetArgs>),
    /// Add a magnet, URL or .torrent handed over by the browser or file manager.
    #[command(after_help = "\
Examples:
  dman handle 'magnet:?xt=urn:btih:...'
  dman handle ~/Downloads/ubuntu.torrent
  dman handle file:///home/me/Downloads/ubuntu.torrent

This is what `dman integrate install` registers for magnet links and .torrent
files. It starts the daemon when needed and, as there is usually no terminal,
reports the result as a desktop notification as well.")]
    #[command(display_order = 27)]
    Handle(HandleArgs),
    /// Make dman the handler of magnet links and .torrent files (install, uninstall, status).
    #[command(after_help = "\
Examples:
  dman integrate install       register dman for magnet links and .torrent files
  dman integrate status        show which program opens magnet links now
  dman integrate uninstall     remove what `install` wrote (nothing else)

Linux: writes ~/.local/share/applications/dman-handler.desktop and makes it the
default with xdg-mime. Windows: writes HKCU registry entries with reg.exe. macOS
needs an app bundle for a URL scheme; `install` explains the alternatives.
Browser bookmarklet and userscript: see docs/browser.md.")]
    #[command(display_order = 28)]
    Integrate {
        #[command(subcommand)]
        cmd: IntegrateCmd,
    },
    /// List downloads.
    #[command(after_help = "\
Examples:
  dman list
  dman list --state active,seeding
  dman list --watch
  dman list --sort speed --limit 5
  dman list --name ubuntu --engine aria2 --columns id,name,progress
  dman list --json | jq '.[].name'")]
    #[command(display_order = 2)]
    List(ListArgs),
    /// Wait until downloads finish (exit 0 if all completed).
    #[command(after_help = "\
Examples:
  dman wait 12
  dman wait 3 4 5 --timeout 30m
  dman add https://example.org/big.iso --wait && ./next-step.sh

A torrent counts as finished once it completes (seeding does not delay the
exit). Exit codes: 0 all completed, 1 every download failed, 8 some failed,
124 the timeout expired (the downloads keep running), 130 Ctrl-C. Waiting
never changes the downloads.")]
    #[command(display_order = 4)]
    Wait(WaitArgs),
    /// Show details of one download.
    #[command(after_help = "\
Examples:
  dman get 12
  dman get 12 --files
  dman get 12 --watch            live progress bar until the download ends")]
    #[command(display_order = 3)]
    Get {
        /// Download ID.
        #[arg(add = comp::job_ids_any())]
        id: JobId,
        /// Redraw a progress bar (speed, ETA, peers) until the download ends; plain
        /// lines every few seconds when stderr is not a terminal.
        #[arg(long, short = 'w')]
        watch: bool,
        /// Also list the files.
        #[arg(long)]
        files: bool,
        /// Also list the connected peers (active torrents).
        #[arg(long)]
        peers: bool,
    },
    /// Rename a download (waiting/paused: new output name; complete: move on disk).
    #[command(after_help = "\
Examples:
  dman rename 12 ubuntu.iso
  dman rename 12 season-1             a finished multi-file torrent: its folder
  dman rename 12 --file 3 notes.txt   one file of a multi-file download")]
    #[command(display_order = 10)]
    Rename {
        /// Download ID.
        #[arg(add = comp::job_ids_any())]
        id: JobId,
        /// New file name (no `/`); for a completed multi-file torrent without
        /// --file, the new top-level directory name.
        name: String,
        /// File of a multi-file download (1-based).
        #[arg(long, value_name = "N")]
        file: Option<u32>,
    },
    /// Delete the partial files of a finished torrent's deselected files (asks first).
    #[command(after_help = "\
Examples:
  dman cleanup 12          list the files, then ask
  dman cleanup 12 --yes    delete without asking (scripts)")]
    #[command(display_order = 12)]
    Cleanup {
        /// Download ID.
        #[arg(add = comp::job_ids_finished())]
        id: JobId,
        /// Do not ask for confirmation.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Show or change settings (get, set, edit, show, path, reload).
    #[command(after_help = "\
Examples:
  dman config get limits.download
  dman config set limits.download 5MiB
  dman config edit")]
    #[command(display_order = 23)]
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Video quality picker defaults (`dman video prefs`).
    #[command(after_help = "\
Examples:
  dman video prefs
  dman video prefs --quality 1080p --ask false")]
    #[command(display_order = 24)]
    Video {
        #[command(subcommand)]
        cmd: VideoCmd,
    },
    /// Show the formats, subtitles and quality plans of a video URL (yt-dlp), or with
    /// --plan what `dman add` would do.
    #[command(after_help = "\
Examples:
  dman probe https://youtu.be/ID                 formats, subtitles and quality plans
  dman probe https://youtu.be/ID --json          the same as stable JSON
  dman probe https://youtu.be/ID --plan          the yt-dlp selector and post-processing add would use
  dman probe https://youtu.be/ID --plan --quality 720p --container mkv --subs en")]
    #[command(display_order = 15)]
    Probe {
        /// Video page or stream URL.
        url: String,
        /// List the playlist entries instead of probing one video (with --plan: plan the
        /// whole playlist).
        #[arg(long)]
        playlist: bool,
        /// Read cookies from this browser (firefox, chrome, ...).
        #[arg(long, value_name = "BROWSER", add = comp::browser())]
        cookies_from_browser: Option<String>,
        /// Print what `dman add` would do for the saved preferences and the flags below:
        /// the exact yt-dlp format selector, the post-processing, the ffmpeg fallback and
        /// whether the quality picker would ask. Nothing is probed or downloaded.
        #[arg(long)]
        plan: bool,
        #[command(flatten)]
        plan_args: PlanArgs,
    },
    /// Show the files of a magnet link (or http(s) .torrent URL) without downloading it.
    #[command(after_help = "\
Examples:
  dman preview 'magnet:?xt=urn:btih:...'
  dman preview https://example.org/ubuntu.torrent
  dman preview 'magnet:?xt=urn:btih:...' --timeout 30 --json")]
    #[command(display_order = 14)]
    Preview {
        /// Magnet link, or the http(s) URL of a .torrent file.
        magnet: String,
        /// Give up after this many seconds (default 120).
        #[arg(long, value_name = "SECS")]
        timeout: Option<u32>,
    },
    /// Pause downloads.
    #[command(after_help = "\
Examples:
  dman pause 12
  dman pause 12 13 14
  dman pause --all")]
    #[command(display_order = 5)]
    Pause(TargetArgs),
    /// Resume downloads.
    #[command(after_help = "\
Examples:
  dman resume 12
  dman resume --all")]
    #[command(display_order = 6)]
    Resume(TargetArgs),
    /// Remove downloads (files are kept unless --delete-files).
    #[command(after_help = "\
Examples:
  dman remove 12                       asks first; `dman restore 12` undoes it
  dman remove 12 13 --yes              without asking (scripts)
  dman remove 12 --delete-files        also delete the downloaded files (cannot be undone)")]
    #[command(display_order = 8)]
    Remove(RemoveArgs),
    /// Undo a remove: bring removed downloads back (not possible after --delete-files).
    #[command(after_help = "\
Examples:
  dman restore 12
  dman list --state removed      see what can be restored")]
    #[command(display_order = 9)]
    Restore(RestoreArgs),
    /// Drop finished, failed and removed downloads from the history (files are kept).
    #[command(after_help = "\
Examples:
  dman purge             every finished, failed and removed download (asks first)
  dman purge 12 13 -y    these two, without asking")]
    #[command(display_order = 13)]
    Purge(PurgeArgs),
    /// Change a download's queue position.
    #[command(after_help = "\
Examples:
  dman reorder 12 --top
  dman reorder 12 --to 3
  dman reorder 12 --down")]
    #[command(display_order = 11)]
    Reorder(ReorderArgs),
    /// Show or set the global speed limits.
    #[command(after_help = "\
Examples:
  dman limit                            show the limits in force
  dman limit --download 5MiB
  dman limit --download 0 --upload 0    remove both limits")]
    #[command(display_order = 19)]
    Limit(LimitArgs),
    /// Show or set the number of concurrent downloads.
    #[command(after_help = "\
Examples:
  dman max-concurrent
  dman max-concurrent 5")]
    #[command(display_order = 20)]
    MaxConcurrent {
        /// New limit; omit to print the current one.
        n: Option<u32>,
    },
    /// Show the schedule windows, the active one and the next change.
    #[command(after_help = "\
Examples:
  dman schedule
  dman schedule --json

Windows are set in the [[schedule]] tables of config.toml (`dman config edit`).")]
    #[command(display_order = 21)]
    Schedule,
    /// Show global statistics.
    #[command(after_help = "\
Examples:
  dman stats
  dman stats --json")]
    #[command(display_order = 22)]
    Stats,
    /// Print a shell completion script, or install it (`completions install`).
    #[command(after_help = "\
Examples:
  dman completions install                install for the shell in $SHELL
  dman completions install --shell fish   choose the shell explicitly
  dman completions bash > dman.bash       print the script (for package managers)")]
    #[command(display_order = 29)]
    Completions(CompletionsArgs),
    /// Search the enabled providers; optionally add results.
    #[command(after_help = "\
Examples:
  dman search ubuntu 24.04
  dman search 'blade runner' --provider YTS --category movies
  dman search ubuntu --limit 5 --add 1,2   add the first two results
  dman search ubuntu --no-prompt           never ask which to add (default in scripts)
  dman search --history

In a terminal (and without --add or --json) the results are followed by the
question \"Add which?\"; answer with numbers such as 1,3 or 2-4, Enter skips.")]
    #[command(display_order = 16)]
    Search(Box<SearchArgs>),
    /// Manage search providers (enable, disable, test, import, export).
    #[command(after_help = "\
Examples:
  dman providers list
  dman providers enable YTS Nyaa
  dman providers test YTS")]
    #[command(display_order = 17)]
    Providers {
        #[command(subcommand)]
        cmd: ProvidersCmd,
    },
    /// Public tracker list status.
    #[command(after_help = "\
Examples:
  dman trackers status")]
    #[command(display_order = 18)]
    Trackers {
        #[command(subcommand)]
        cmd: TrackersCmd,
    },
    /// Serve the browser UI (enable it on the running daemon, or run a daemon with it).
    #[command(after_help = "\
Examples:
  dman serve                              web UI on http://127.0.0.1:6811
  dman serve --bind 0.0.0.0 --persist     expose it on the network and remember that
  dman serve --base-path /dman            behind a reverse proxy at /dman
  dman serve --stop                       turn the web UI off again")]
    #[command(display_order = 26)]
    Serve(ServeArgs),
    /// Update dman (installer installs only) or show the right command for your install.
    #[command(after_help = "\
Examples:
  dman update --check     show the installed and the latest version
  dman update             update (asks first)")]
    #[command(display_order = 30)]
    Update(UpdateArgs),
}

#[derive(Args, Debug)]
#[command(args_conflicts_with_subcommands = true)]
pub struct CompletionsArgs {
    /// Print the completion script of this shell to stdout.
    pub shell: Option<Shell>,
    #[command(subcommand)]
    pub cmd: Option<CompletionsCmd>,
}

#[derive(Subcommand, Debug)]
pub enum CompletionsCmd {
    /// Write the completion script where your shell loads it and say what else to do.
    #[command(after_help = "\
Examples:
  dman completions install
  dman completions install --shell zsh")]
    Install {
        /// Shell to install for (default: detected from $SHELL).
        #[arg(long, value_enum)]
        shell: Option<InstallShell>,
        /// Replace an existing, different completion file.
        #[arg(long)]
        force: bool,
    },
}

/// Shells `completions install` knows a standard location for.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallShell {
    Bash,
    Zsh,
    Fish,
}

impl InstallShell {
    pub fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Self::Bash, Self::Zsh, Self::Fish]
            .into_iter()
            .find(|s| s.name() == name)
    }
}

#[derive(Args, Debug)]
pub struct UpdateArgs {
    /// Only report the installed and latest version; change nothing.
    #[arg(long)]
    pub check: bool,
    /// Do not ask: restart a running daemon after the update.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Args, Debug, Default)]
pub struct SearchArgs {
    /// Query words.
    #[arg(required_unless_present_any = ["history", "clear_history"])]
    pub query: Vec<String>,
    /// Only this provider (repeatable); default: every enabled provider.
    #[arg(short, long = "provider", value_name = "NAME", add = comp::providers_any())]
    pub provider: Vec<String>,
    /// Category filter (movies, tv, audio, software, books, xxx, other).
    #[arg(long, value_name = "CATEGORY", add = comp::category())]
    pub category: Option<String>,
    /// Add these results (the numbers of the table, e.g. 1,3) to the downloads.
    #[arg(long, value_name = "N[,M]", value_delimiter = ',', value_parser = parse_index)]
    pub add: Vec<usize>,
    /// Show at most this many results (0 = all).
    #[arg(long, value_name = "N", default_value_t = 20)]
    pub limit: usize,
    /// Do not ask which results to add, even in a terminal.
    #[arg(long)]
    pub no_prompt: bool,
    /// Show the recent queries instead of searching.
    #[arg(long, conflicts_with_all = ["query", "add"])]
    pub history: bool,
    /// Forget the recent queries.
    #[arg(long, conflicts_with_all = ["query", "add", "history"])]
    pub clear_history: bool,
}

/// A 1-based result number.
fn parse_index(s: &str) -> Result<usize, String> {
    match s.trim().parse::<usize>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(format!("`{s}` is not a result number (1 or more)")),
    }
}

#[derive(Subcommand, Debug)]
pub enum ProvidersCmd {
    /// List providers with their state.
    #[command(after_help = "\
Examples:
  dman providers list
  dman providers list --json")]
    List,
    /// Enable providers (nothing is searched until one is enabled).
    #[command(after_help = "\
Examples:
  dman providers enable YTS
  dman providers enable YTS Nyaa")]
    Enable {
        /// Provider names.
        #[arg(required = true, add = comp::providers_disabled())]
        names: Vec<String>,
    },
    /// Disable providers.
    #[command(after_help = "\
Examples:
  dman providers disable YTS")]
    Disable {
        /// Provider names.
        #[arg(required = true, add = comp::providers_enabled())]
        names: Vec<String>,
    },
    /// Probe a provider with a test query and show what it returns (works while disabled).
    #[command(after_help = "\
Examples:
  dman providers test YTS")]
    Test {
        /// Provider name.
        #[arg(add = comp::providers_any())]
        name: String,
    },
    /// Add or replace custom providers from a TOML file (`-` = stdin).
    #[command(after_help = "\
Examples:
  dman providers import my-providers.toml
  dman providers export Mine | dman providers import -")]
    Import {
        /// File of `[[provider]]` tables.
        #[arg(add = comp::any_file())]
        file: String,
    },
    /// Print providers as TOML (default: the custom ones).
    #[command(after_help = "\
Examples:
  dman providers export > my-providers.toml
  dman providers export YTS")]
    Export {
        /// Provider names.
        #[arg(add = comp::providers_any())]
        names: Vec<String>,
    },
    /// Remove a custom provider (built-ins can only be disabled).
    #[command(after_help = "\
Examples:
  dman providers remove Mine")]
    Remove {
        /// Provider name.
        #[arg(add = comp::providers_custom())]
        name: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum TrackersCmd {
    /// Show whether the tracker list is enabled, how many trackers are loaded and when.
    #[command(after_help = "\
Examples:
  dman trackers status
  dman trackers status --json")]
    Status,
}

#[derive(Subcommand, Debug)]
pub enum DaemonCmd {
    /// Run the daemon in the foreground (for systemd units, containers and debugging).
    #[command(after_help = "\
Examples:
  dman daemon run          logs go to stderr; Ctrl-C stops it
  dman daemon run -v       more detail in the log")]
    Run {
        /// Detach from the controlling terminal (used internally).
        #[arg(long, hide = true)]
        detach: bool,
    },
    /// Start the daemon in the background.
    #[command(after_help = "\
Examples:
  dman daemon start        start it (no-op if it is already running)
  dman daemon status       check that it runs and which engines it found
  dman daemon gen-config   write a commented config.toml to edit first")]
    Start,
    /// Stop the daemon.
    #[command(after_help = "\
Examples:
  dman daemon stop")]
    Stop,
    /// Stop and start the daemon.
    #[command(after_help = "\
Examples:
  dman daemon restart      e.g. after changing a setting that needs a restart")]
    Restart,
    /// Show daemon, engine and transfer status.
    #[command(after_help = "\
Examples:
  dman daemon status
  dman daemon status --json")]
    Status,
    /// Start (or cleanly restart) the aria2 engine, e.g. after installing it.
    #[command(after_help = "\
Examples:
  dman daemon engine-restart")]
    EngineRestart,
    /// Write the default config file if none exists.
    #[command(after_help = "\
Examples:
  dman daemon gen-config   then `dman config edit` to change it")]
    GenConfig,
}

#[derive(Args, Debug, Default)]
pub struct ServeArgs {
    /// Address to bind (default from `[web]`: 127.0.0.1). Non-loopback exposes the UI.
    #[arg(long, value_name = "ADDR")]
    pub bind: Option<String>,
    /// TCP port (default from `[web]`: 6811).
    #[arg(long, value_name = "N")]
    pub port: Option<u16>,
    /// Mount point behind a reverse proxy, e.g. /dman.
    #[arg(long, value_name = "PATH")]
    pub base_path: Option<String>,
    /// Write these settings (and enabled = true) to `[web]` in config.toml.
    #[arg(long)]
    pub persist: bool,
    /// Stop the web UI of the running daemon (with --persist: also disable it in config.toml).
    #[arg(
        long,
        conflicts_with_all = ["bind", "port", "base_path", "print_token"]
    )]
    pub stop: bool,
    /// Print the access token (secret!) after starting. Never printed otherwise; only
    /// network binds (or `require_login`) use one.
    #[arg(long)]
    pub print_token: bool,
}

#[derive(Args, Debug, Default)]
pub struct AddArgs {
    /// URLs, magnets, local .torrent/.metalink/.meta4 files, or `-` for stdin.
    #[arg(add = comp::add_sources())]
    pub sources: Vec<String>,
    /// Read sources from a text file (one per line; blank lines and # comments skipped).
    #[arg(short = 'i', long, value_name = "FILE", add = comp::any_file())]
    pub input_file: Option<String>,
    /// Treat all URI sources as mirrors of ONE download.
    #[arg(long)]
    pub mirror: bool,
    /// Download directory (on the daemon host).
    #[arg(long, value_name = "DIR", add = comp::any_dir())]
    pub dir: Option<String>,
    /// Output file name.
    #[arg(long, value_name = "NAME")]
    pub out: Option<String>,
    /// Add paused.
    #[arg(long)]
    pub paused: bool,
    /// Download speed limit for this download, e.g. 2MiB.
    #[arg(long, value_name = "SIZE", value_parser = parse_size)]
    pub max_download_limit: Option<u64>,
    /// Upload speed limit for this download, e.g. 500K.
    #[arg(long, value_name = "SIZE", value_parser = parse_size)]
    pub max_upload_limit: Option<u64>,
    /// Files to download (1-based), e.g. 1,3-5.
    #[arg(long, value_name = "LIST", value_parser = crate::commands::add::parse_select_files)]
    // Spelled out so clap parses one LIST into the whole list instead of
    // treating the option as one that takes several values.
    pub select_files: Option<std::vec::Vec<u32>>,
    /// Stop seeding at this share ratio.
    #[arg(long, value_name = "RATIO")]
    pub seed_ratio: Option<f64>,
    /// Stop seeding after this many minutes.
    #[arg(long, value_name = "MINUTES")]
    pub seed_time: Option<u64>,
    /// Download torrent pieces in order.
    #[arg(long)]
    pub sequential: bool,
    /// HTTP header (repeatable), e.g. "Cookie: a=b".
    #[arg(long, value_name = "HEADER")]
    pub header: Vec<String>,
    /// User-Agent header.
    #[arg(long, value_name = "UA")]
    pub user_agent: Option<String>,
    /// Referer header.
    #[arg(long, value_name = "URL")]
    pub referer: Option<String>,
    /// Proxy URL.
    #[arg(long, value_name = "URL")]
    pub proxy: Option<String>,
    /// Expected checksum, `<algo>=<hex>`.
    #[arg(long, value_name = "ALGO=HEX")]
    pub checksum: Option<String>,
    /// Split each download into N segments.
    #[arg(long, value_name = "N")]
    pub split: Option<u32>,
    /// Maximum connections per server.
    #[arg(long, value_name = "N")]
    pub connections: Option<u32>,
    /// Raw engine option (repeatable).
    #[arg(short = 'o', long = "option", value_name = "KEY=VALUE")]
    pub options: Vec<String>,
    /// Never ask: videos use the saved quality defaults (also implied by pipes and --json).
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Add the URL or magnet link in the clipboard (read with wl-paste, xclip, xsel,
    /// pbpaste or PowerShell). Without sources, a terminal offers it on its own.
    #[arg(long)]
    pub clipboard: bool,
    /// Add even if the same URL or torrent is already in the downloads.
    #[arg(long)]
    pub force: bool,
    /// Wait until the added downloads finish, showing progress on stderr (exit 0
    /// if all completed, 124 on --timeout).
    #[arg(long, conflicts_with = "paused")]
    pub wait: bool,
    /// With --wait: give up after this long, e.g. 90s, 15m, 2h (the downloads keep running).
    #[arg(long, requires = "wait", value_name = "DURATION", value_parser = crate::commands::wait::parse_duration)]
    pub timeout: Option<std::time::Duration>,
    #[command(flatten)]
    pub video: VideoArgs,
}

/// `dman set`: the options to change; unset ones stay as they are.
#[derive(Args, Debug, Default)]
#[command(override_usage = "dman set [OPTIONS] <IDS>... <--OPTION VALUE>...")]
#[command(group(clap::ArgGroup::new("change").required(true).multiple(true)))]
pub struct SetArgs {
    /// Download IDs.
    #[arg(required = true, add = comp::job_ids_unfinished())]
    pub ids: Vec<JobId>,
    /// Download speed limit, e.g. 2MiB (0 = unlimited).
    #[arg(long, value_name = "SIZE", value_parser = parse_size, group = "change")]
    pub max_download_limit: Option<u64>,
    /// Upload speed limit, e.g. 500K (0 = unlimited).
    #[arg(long, value_name = "SIZE", value_parser = parse_size, group = "change")]
    pub max_upload_limit: Option<u64>,
    /// Download directory (on the daemon host); only while waiting or paused.
    #[arg(long, value_name = "DIR", add = comp::any_dir(), group = "change")]
    pub dir: Option<String>,
    /// Files of a torrent to download (1-based), e.g. 1,3-5.
    #[arg(
        long,
        visible_alias = "select-files",
        value_name = "LIST",
        value_parser = crate::commands::add::parse_select_files,
        group = "change"
    )]
    pub select: Option<std::vec::Vec<u32>>,
    /// Stop seeding at this share ratio.
    #[arg(long, value_name = "RATIO", group = "change")]
    pub seed_ratio: Option<f64>,
    /// Stop seeding after this many minutes.
    #[arg(long, value_name = "MINUTES", group = "change")]
    pub seed_time: Option<u64>,
    /// HTTP header (repeatable; replaces the download's headers), e.g. "Cookie: a=b".
    #[arg(long, value_name = "HEADER", group = "change")]
    pub header: Vec<String>,
    /// User-Agent header.
    #[arg(long, value_name = "UA", group = "change")]
    pub user_agent: Option<String>,
    /// Referer header.
    #[arg(long, value_name = "URL", group = "change")]
    pub referer: Option<String>,
    /// Maximum connections per server.
    #[arg(long, value_name = "N", group = "change")]
    pub connections: Option<u32>,
    /// Split each download into N segments.
    #[arg(long, value_name = "N", group = "change")]
    pub split: Option<u32>,
    /// Raw engine option (repeatable).
    #[arg(
        short = 'o',
        long = "option",
        value_name = "KEY=VALUE",
        group = "change"
    )]
    pub options: Vec<String>,
    /// Queue position (0 = next to start); several IDs keep their order from there.
    #[arg(long, value_name = "N", group = "change")]
    pub position: Option<u32>,
}

/// `dman handle`.
#[derive(Args, Debug)]
pub struct HandleArgs {
    /// Magnet link, URL, .torrent/.metalink path or file:// URL.
    #[arg(required = true, add = comp::add_sources())]
    pub sources: Vec<String>,
}

#[derive(Subcommand, Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrateCmd {
    /// Register dman for magnet links and .torrent files and make it the default.
    #[command(after_help = "\
Examples:
  dman integrate install")]
    Install,
    /// Remove the registration `install` wrote (only files and keys dman created).
    #[command(after_help = "\
Examples:
  dman integrate uninstall")]
    Uninstall,
    /// Show the current handler of magnet links and .torrent files.
    #[command(after_help = "\
Examples:
  dman integrate status
  dman integrate status --json")]
    Status,
}

/// The `dman add` video flags that change a plan (`dman probe --plan`).
#[derive(Args, Debug, Default)]
#[command(next_help_heading = "With --plan")]
pub struct PlanArgs {
    /// Quality: best, 2160p, 1440p, 1080p, 720p, 480p, audio:m4a|mp3|opus, or a yt-dlp selector.
    #[arg(long, value_name = "Q", requires = "plan", add = comp::quality())]
    pub quality: Option<String>,
    /// Exact format id(s) from `dman probe`, e.g. 137+140.
    #[arg(long, value_name = "ID", requires = "plan")]
    pub format_id: Option<String>,
    /// Container for merged video: mp4, mkv or webm.
    #[arg(long, value_name = "C", requires = "plan", add = comp::container())]
    pub container: Option<String>,
    /// Playlist items, e.g. 1-3,7.
    #[arg(long, value_name = "LIST", requires = "plan")]
    pub playlist_items: Option<String>,
    /// Subtitle languages, e.g. en,de.
    #[arg(long, value_name = "LANGS", value_delimiter = ',', requires = "plan")]
    pub subs: Vec<String>,
    /// Embed the subtitles into the video (needs ffmpeg).
    #[arg(long, requires = "plan")]
    pub embed_subs: bool,
    /// Also fetch automatic captions.
    #[arg(long, requires = "plan")]
    pub auto_subs: bool,
    /// Embed the thumbnail (needs ffmpeg).
    #[arg(long, requires = "plan")]
    pub thumbnail: bool,
    /// Do not embed metadata.
    #[arg(long, requires = "plan")]
    pub no_metadata: bool,
    /// Cut SponsorBlock segments, e.g. sponsor,intro (needs ffmpeg).
    #[arg(long, value_name = "CATS", value_delimiter = ',', requires = "plan", add = comp::sponsorblock())]
    pub sponsorblock_remove: Vec<String>,
    /// Without ffmpeg, accept video and audio as separate files instead of a lower quality.
    #[arg(long, requires = "plan")]
    pub allow_separate_streams: bool,
}

/// Video options of `dman add` (yt-dlp engine).
#[derive(Args, Debug, Default)]
#[command(next_help_heading = "Video (yt-dlp)")]
pub struct VideoArgs {
    /// Download the URL(s) with yt-dlp as video (detected automatically for known video sites).
    #[arg(long, conflicts_with = "no_video")]
    pub video: bool,
    /// Never treat a URL as video; download the page or file itself.
    #[arg(long)]
    pub no_video: bool,
    /// Quality: best, 2160p, 1440p, 1080p, 720p, 480p, audio:m4a|mp3|opus, or a yt-dlp selector.
    #[arg(long, value_name = "Q", add = comp::quality())]
    pub quality: Option<String>,
    /// Exact format id(s) from `dman probe`, e.g. 137+140.
    #[arg(long, value_name = "ID")]
    pub format_id: Option<String>,
    /// Container for merged video: mp4, mkv or webm.
    #[arg(long, value_name = "C", add = comp::container())]
    pub container: Option<String>,
    /// Download the whole playlist.
    #[arg(long)]
    pub playlist: bool,
    /// Playlist items, e.g. 1-3,7 (implies --playlist).
    #[arg(long, value_name = "LIST")]
    pub playlist_items: Option<String>,
    /// Subtitle languages, e.g. en,de.
    #[arg(long, value_name = "LANGS", value_delimiter = ',')]
    pub subs: Vec<String>,
    /// Embed the subtitles into the video (needs ffmpeg).
    #[arg(long)]
    pub embed_subs: bool,
    /// Also fetch automatic captions.
    #[arg(long)]
    pub auto_subs: bool,
    /// Embed the thumbnail (needs ffmpeg).
    #[arg(long)]
    pub thumbnail: bool,
    /// Do not embed metadata.
    #[arg(long)]
    pub no_metadata: bool,
    /// Cut SponsorBlock segments, e.g. sponsor,intro (needs ffmpeg).
    #[arg(long, value_name = "CATS", value_delimiter = ',', add = comp::sponsorblock())]
    pub sponsorblock_remove: Vec<String>,
    /// Read cookies from this browser (firefox, chrome, ...).
    #[arg(long, value_name = "BROWSER", add = comp::browser())]
    pub cookies_from_browser: Option<String>,
    /// Without ffmpeg, accept video and audio as separate files instead of a lower quality.
    #[arg(long)]
    pub allow_separate_streams: bool,
    /// Output template, e.g. "%(uploader)s/%(title)s.%(ext)s".
    #[arg(long, value_name = "TEMPLATE")]
    pub template: Option<String>,
    /// Extra yt-dlp argument (repeatable; allow-listed by the daemon).
    #[arg(long = "ytdlp-arg", value_name = "ARG", allow_hyphen_values = true)]
    pub ytdlp_args: Vec<String>,
}

impl VideoArgs {
    /// Any video-only option was given.
    pub fn any(&self) -> bool {
        self.quality.is_some()
            || self.format_id.is_some()
            || self.container.is_some()
            || self.playlist
            || self.playlist_items.is_some()
            || !self.subs.is_empty()
            || self.embed_subs
            || self.auto_subs
            || self.thumbnail
            || self.no_metadata
            || !self.sponsorblock_remove.is_empty()
            || self.cookies_from_browser.is_some()
            || self.allow_separate_streams
            || self.template.is_some()
            || !self.ytdlp_args.is_empty()
    }
}

/// Parse a size such as `2MiB` or `500K` into bytes.
pub fn parse_size(s: &str) -> Result<u64, String> {
    dman_config::parse_bytes(s)
}

/// States accepted by `list --state`.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateArg {
    Active,
    Waiting,
    Paused,
    Seeding,
    Complete,
    Error,
    Removed,
}

impl From<StateArg> for JobState {
    fn from(s: StateArg) -> Self {
        match s {
            StateArg::Active => JobState::Active,
            StateArg::Waiting => JobState::Waiting,
            StateArg::Paused => JobState::Paused,
            StateArg::Seeding => JobState::Seeding,
            StateArg::Complete => JobState::Complete,
            StateArg::Error => JobState::Error,
            StateArg::Removed => JobState::Removed,
        }
    }
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Only these states (comma-separated).
    #[arg(long, value_enum, value_delimiter = ',')]
    pub state: Vec<StateArg>,
    /// Only downloads whose name contains this text (case-insensitive).
    #[arg(long, value_name = "TEXT")]
    pub name: Option<String>,
    /// Only downloads of this engine.
    #[arg(long, value_enum, value_name = "ENGINE")]
    pub engine: Option<EngineArg>,
    /// Sort by this key (speed, size, progress, added: highest/newest first; eta:
    /// soonest first; name: A-Z).
    #[arg(long, value_enum, value_name = "KEY")]
    pub sort: Option<SortKey>,
    /// Reverse the sort order.
    #[arg(long)]
    pub reverse: bool,
    /// Show at most this many downloads (after filtering and sorting).
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
    /// Columns of the table (comma-separated); ignored with --json.
    #[arg(long, value_enum, value_delimiter = ',', value_name = "LIST")]
    pub columns: Vec<crate::format::Column>,
    /// Keep running and redraw on every update (Ctrl-C to stop).
    #[arg(long)]
    pub watch: bool,
}

/// Engine filter of `dman list`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum EngineArg {
    Aria2,
    Ytdlp,
}

/// Sort keys of `dman list`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum SortKey {
    Speed,
    Eta,
    Size,
    Added,
    Name,
    Progress,
}

#[derive(Args, Debug)]
pub struct WaitArgs {
    /// Download IDs to wait for.
    #[arg(required = true, add = comp::job_ids_running())]
    pub ids: Vec<JobId>,
    /// Give up after this long, e.g. 90s, 15m, 2h, 1h30m (exit 124; the downloads keep running).
    #[arg(long, value_name = "DURATION", value_parser = crate::commands::wait::parse_duration)]
    pub timeout: Option<std::time::Duration>,
}

#[derive(Args, Debug)]
pub struct TargetArgs {
    /// Download IDs.
    #[arg(required_unless_present = "all", add = comp::job_ids_running())]
    pub ids: Vec<JobId>,
    /// Apply to every download.
    #[arg(long, conflicts_with = "ids")]
    pub all: bool,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCmd {
    /// Re-read config.toml in the running daemon and apply what can change live
    /// (limits, schedule, notifications, search, torrents, video defaults).
    #[command(after_help = "\
Examples:
  dman config reload       after editing config.toml by hand")]
    Reload,
    /// Print one setting (or a whole section) of the effective configuration.
    #[command(after_help = "\
Examples:
  dman config get limits.download
  dman config get search          a whole section, as TOML
  dman config get --json ui.theme

Secrets (tokens, passwords, proxy credentials, webhook URLs) are shown as <redacted>.")]
    Get {
        /// Dotted key such as `limits.download` or `search.dns`.
        #[arg(value_name = "KEY", add = comp::config_keys())]
        key: String,
    },
    /// Change one setting in config.toml (comments and layout are kept).
    #[command(after_help = "\
Examples:
  dman config set limits.download 5MiB
  dman config set ui.mouse true
  dman config set profiles.local.download_dir ~/Downloads/dman
  dman config set search.dns doh:cloudflare --no-reload

The whole file is checked before it is saved; a bad value leaves it untouched. A running
daemon is reloaded afterwards and told which settings need `dman daemon restart`.")]
    Set {
        /// Dotted key such as `limits.download`.
        #[arg(value_name = "KEY", add = comp::config_keys())]
        key: String,
        /// New value: a number, true/false, a list like ["a","b"], or text.
        #[arg(value_name = "VALUE", allow_hyphen_values = true)]
        value: String,
        /// Only write the file; do not reload the daemon.
        #[arg(long)]
        no_reload: bool,
    },
    /// Remove a setting from config.toml so its default applies again.
    #[command(after_help = "\
Examples:
  dman config unset limits.download")]
    Unset {
        /// Dotted key such as `limits.download`.
        #[arg(value_name = "KEY", add = comp::config_keys())]
        key: String,
        /// Only write the file; do not reload the daemon.
        #[arg(long)]
        no_reload: bool,
    },
    /// Edit config.toml in $VISUAL / $EDITOR; it is checked before it replaces the file.
    #[command(after_help = "\
Examples:
  dman config edit
  EDITOR='code -w' dman config edit

The editor works on a copy. If the result is not a valid configuration you are asked
whether to open the editor again; declining discards the edit and keeps the file.")]
    Edit {
        /// Only write the file; do not reload the daemon.
        #[arg(long)]
        no_reload: bool,
    },
    /// Print the path of config.toml.
    #[command(after_help = "\
Examples:
  dman config path")]
    Path,
    /// Print the effective configuration (file plus defaults) with secrets redacted.
    #[command(after_help = "\
Examples:
  dman config show
  dman config show --json")]
    Show,
}

#[derive(Subcommand, Debug)]
pub enum VideoCmd {
    /// Show or change the quality picker settings (no flags = show).
    #[command(after_help = "\
Examples:
  dman video prefs                        show the settings
  dman video prefs --ask false            never show the picker; use the defaults
  dman video prefs --ask true             ask before every video download again
  dman video prefs --quality 1080p --audio m4a --container mp4")]
    Prefs(PrefsArgs),
}

#[derive(Args, Debug, Default)]
pub struct PrefsArgs {
    /// Show the quality picker before downloading a video (true) or use the defaults (false).
    #[arg(long, value_name = "BOOL")]
    pub ask: Option<bool>,
    /// Default quality: best, 2160p, 1440p, 1080p, 720p, 480p, audio:m4a|mp3|opus.
    #[arg(long, value_name = "Q", add = comp::quality())]
    pub quality: Option<String>,
    /// Preferred audio when merging: best, m4a, opus or mp3.
    #[arg(long, value_name = "A", add = comp::audio())]
    pub audio: Option<String>,
    /// Default container: mp4, mkv, webm, or `none` for yt-dlp's choice.
    #[arg(long, value_name = "C", add = comp::container())]
    pub container: Option<String>,
}

#[derive(Args, Debug)]
pub struct RemoveArgs {
    /// Download IDs.
    #[arg(required = true, add = comp::job_ids_any())]
    pub ids: Vec<JobId>,
    /// Also delete the downloaded files (asks once; non-interactive use needs --yes).
    #[arg(long)]
    pub delete_files: bool,
    /// Remove even if the engine does not answer a graceful stop.
    #[arg(long)]
    pub force: bool,
    /// Do not ask for confirmation.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Args, Debug)]
pub struct RestoreArgs {
    /// IDs of removed downloads.
    #[arg(required = true, add = comp::job_ids_removed())]
    pub ids: Vec<JobId>,
}

#[derive(Args, Debug)]
pub struct PurgeArgs {
    /// Download IDs; none = every finished, failed and removed download.
    #[arg(add = comp::job_ids_stopped())]
    pub ids: Vec<JobId>,
    /// Do not ask for confirmation.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Args, Debug)]
#[command(group(clap::ArgGroup::new("move").required(true).multiple(false)))]
#[command(override_usage = "dman reorder [OPTIONS] <ID> <--to <N>|--up|--down|--top|--bottom>")]
pub struct ReorderArgs {
    /// Download ID.
    #[arg(add = comp::job_ids_any())]
    pub id: JobId,
    /// Move to this absolute position (0 = first).
    #[arg(long, value_name = "N", group = "move")]
    pub to: Option<u32>,
    /// Move one place towards the front.
    #[arg(long, group = "move")]
    pub up: bool,
    /// Move one place towards the back.
    #[arg(long, group = "move")]
    pub down: bool,
    /// Move to the front.
    #[arg(long, group = "move")]
    pub top: bool,
    /// Move to the back.
    #[arg(long, group = "move")]
    pub bottom: bool,
}

#[derive(Args, Debug)]
pub struct LimitArgs {
    /// Global download limit (0 = unlimited), e.g. 5MiB.
    #[arg(long, value_name = "SIZE", value_parser = parse_size)]
    pub download: Option<u64>,
    /// Global upload limit (0 = unlimited).
    #[arg(long, value_name = "SIZE", value_parser = parse_size)]
    pub upload: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("dman").chain(args.iter().copied()))
    }

    fn search(args: &[&str]) -> SearchArgs {
        match parse(args).unwrap().command {
            Some(Command::Search(a)) => *a,
            other => panic!("not a search: {other:?}"),
        }
    }

    #[test]
    fn probe_plan_flags_need_plan() {
        match parse(&["probe", "https://e.com/v", "--plan", "--quality", "720p"])
            .unwrap()
            .command
        {
            Some(Command::Probe {
                plan: true,
                plan_args,
                ..
            }) => assert_eq!(plan_args.quality.as_deref(), Some("720p")),
            other => panic!("not a plan: {other:?}"),
        }
        assert!(parse(&["probe", "https://e.com/v", "--quality", "720p"]).is_err());
        assert!(parse(&["probe", "https://e.com/v", "--playlist"]).is_ok());
    }

    #[test]
    fn video_prefs_parse() {
        let prefs = |args: &[&str]| match parse(args).unwrap().command {
            Some(Command::Video {
                cmd: VideoCmd::Prefs(a),
            }) => a,
            other => panic!("not video prefs: {other:?}"),
        };
        let a = prefs(&["video", "prefs"]);
        assert_eq!(
            (a.ask, a.quality, a.audio, a.container),
            (None, None, None, None)
        );
        let a = prefs(&[
            "video",
            "prefs",
            "--ask",
            "false",
            "--quality",
            "1080p",
            "--audio",
            "opus",
            "--container",
            "mkv",
        ]);
        assert_eq!(a.ask, Some(false));
        assert_eq!(a.quality.as_deref(), Some("1080p"));
        assert_eq!(a.audio.as_deref(), Some("opus"));
        assert_eq!(a.container.as_deref(), Some("mkv"));
        assert_eq!(prefs(&["video", "prefs", "--ask", "true"]).ask, Some(true));
        assert!(parse(&["video", "prefs", "--ask", "maybe"]).is_err());
        assert!(parse(&["video"]).is_err());
    }

    #[test]
    fn file_selections_parse_as_one_list() {
        match parse(&["add", "magnet:?x", "--select-files", "1,3-4", "--force"])
            .unwrap()
            .command
        {
            Some(Command::Add(a)) => {
                assert_eq!(a.select_files, Some(vec![1, 3, 4]));
                assert!(a.force && !a.clipboard);
            }
            other => panic!("not add: {other:?}"),
        }
        match parse(&["set", "2", "3", "--select", "2-3", "--position", "0"])
            .unwrap()
            .command
        {
            Some(Command::Set(a)) => {
                assert_eq!(a.ids, [JobId(2), JobId(3)]);
                assert_eq!(a.select, Some(vec![2, 3]));
                assert_eq!(a.position, Some(0));
            }
            other => panic!("not set: {other:?}"),
        }
        assert!(parse(&["set", "2"]).is_err(), "a change is required");
        assert!(matches!(
            parse(&["integrate", "status"]).unwrap().command,
            Some(Command::Integrate {
                cmd: IntegrateCmd::Status
            })
        ));
    }

    #[test]
    fn add_yes_flag_parses() {
        let add = |args: &[&str]| match parse(args).unwrap().command {
            Some(Command::Add(a)) => *a,
            other => panic!("not add: {other:?}"),
        };
        assert!(!add(&["add", "https://youtu.be/x"]).yes);
        assert!(add(&["add", "-y", "https://youtu.be/x"]).yes);
        assert!(add(&["add", "https://youtu.be/x", "--yes"]).yes);
    }

    #[test]
    fn search_words_providers_add_limit() {
        let a = search(&[
            "search",
            "ubuntu",
            "24.04",
            "--provider",
            "Nyaa",
            "-p",
            "YTS",
            "--add",
            "1,3",
            "--limit",
            "5",
            "--category",
            "software",
        ]);
        assert_eq!(a.query, ["ubuntu", "24.04"]);
        assert_eq!(a.provider, ["Nyaa", "YTS"]);
        assert_eq!(a.add, [1, 3]);
        assert_eq!(a.limit, 5);
        assert_eq!(a.category.as_deref(), Some("software"));
        assert!(!a.history);
        let a = search(&["search", "x"]);
        assert_eq!((a.limit, a.add.len(), a.provider.len()), (20, 0, 0));
        assert!(parse(&["--json", "search", "x"]).unwrap().json);
    }

    #[test]
    fn search_argument_errors() {
        assert!(parse(&["search"]).is_err(), "query required");
        assert!(parse(&["search", "x", "--add", "0"]).is_err());
        assert!(parse(&["search", "x", "--add", "a"]).is_err());
        assert!(parse(&["search", "x", "--history"]).is_err());
        assert!(parse(&["search", "--history", "--add", "1"]).is_err());
        assert!(search(&["search", "--history"]).history);
        assert!(search(&["search", "--clear-history"]).clear_history);
    }

    #[test]
    fn providers_and_trackers_subcommands() {
        let cmd = |args: &[&str]| match parse(args).unwrap().command {
            Some(Command::Providers { cmd }) => cmd,
            other => panic!("{other:?}"),
        };
        assert!(matches!(cmd(&["providers", "list"]), ProvidersCmd::List));
        assert!(
            matches!(cmd(&["providers", "enable", "Nyaa", "YTS"]), ProvidersCmd::Enable { names } if names == ["Nyaa", "YTS"])
        );
        assert!(matches!(
            cmd(&["providers", "disable", "Nyaa"]),
            ProvidersCmd::Disable { .. }
        ));
        assert!(
            matches!(cmd(&["providers", "test", "Nyaa"]), ProvidersCmd::Test { name } if name == "Nyaa")
        );
        assert!(
            matches!(cmd(&["providers", "import", "f.toml"]), ProvidersCmd::Import { file } if file == "f.toml")
        );
        assert!(
            matches!(cmd(&["providers", "export"]), ProvidersCmd::Export { names } if names.is_empty())
        );
        assert!(
            matches!(cmd(&["providers", "export", "A", "B"]), ProvidersCmd::Export { names } if names.len() == 2)
        );
        assert!(matches!(
            cmd(&["providers", "remove", "x"]),
            ProvidersCmd::Remove { .. }
        ));
        assert!(parse(&["providers", "enable"]).is_err());
        assert!(parse(&["providers"]).is_err());
        assert!(matches!(
            parse(&["trackers", "status"]).unwrap().command,
            Some(Command::Trackers {
                cmd: TrackersCmd::Status
            })
        ));
    }

    #[test]
    fn restore_takes_job_ids() {
        match parse(&["restore", "3", "4"]).unwrap().command {
            Some(Command::Restore(a)) => assert_eq!(a.ids, [JobId(3), JobId(4)]),
            other => panic!("not restore: {other:?}"),
        }
        assert!(parse(&["restore"]).is_err());
        assert!(parse(&["restore", "x"]).is_err());
    }

    #[test]
    fn serve_arguments() {
        let serve = |args: &[&str]| match parse(args).unwrap().command {
            Some(Command::Serve(a)) => a,
            other => panic!("not serve: {other:?}"),
        };
        let a = serve(&["serve"]);
        assert!(a.bind.is_none() && a.port.is_none() && a.base_path.is_none());
        assert!(!a.persist && !a.stop && !a.print_token);
        let a = serve(&[
            "serve",
            "--bind",
            "0.0.0.0",
            "--port",
            "8080",
            "--base-path",
            "/dman",
            "--persist",
            "--print-token",
        ]);
        assert_eq!(a.bind.as_deref(), Some("0.0.0.0"));
        assert_eq!(a.port, Some(8080));
        assert_eq!(a.base_path.as_deref(), Some("/dman"));
        assert!(a.persist && a.print_token);
        assert!(serve(&["serve", "--stop", "--persist"]).stop);
        assert!(parse(&["serve", "--stop", "--port", "1"]).is_err());
        assert!(parse(&["serve", "--port", "70000"]).is_err());
    }
}
