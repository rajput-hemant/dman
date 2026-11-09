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

/// dman: a terminal-first download manager.
///
/// A background daemon owns aria2 (and yt-dlp for video) and all state; this CLI, the
/// interactive UI (run `dman` without arguments) and the browser UI (`dman serve`) are
/// clients of it.
///
/// Tab completion (job IDs with their names, providers, profiles, options) is installed
/// with `dman completions install`; `dman completions <shell>` prints the script instead.
/// Exit codes: 0 ok, 1 general error, 2 usage error, 130 interrupted; see `--json` for
/// machine-readable errors.
#[derive(Parser, Debug)]
#[command(name = "dman", version, propagate_version = true, after_help = EXAMPLES)]
pub struct Cli {
    /// Daemon profile to use.
    #[arg(long, global = true, env = "DMAN_PROFILE", value_name = "NAME", add = comp::profiles())]
    pub profile: Option<String>,
    /// Machine-readable output (JSON), also for errors.
    #[arg(long, global = true)]
    pub json: bool,
    /// Increase log verbosity (-v info, -vv debug, -vvv trace).
    #[arg(short, long, global = true, action = ArgAction::Count)]
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
  dman add https://youtu.be/ID --quality 1080p --container mkv --subs en")]
    Add(Box<AddArgs>),
    /// List downloads.
    #[command(after_help = "\
Examples:
  dman list
  dman list --state active,seeding
  dman list --watch
  dman list --json | jq '.[].name'")]
    List(ListArgs),
    /// Show details of one download.
    Get {
        /// Job ID.
        #[arg(add = comp::job_ids_any())]
        id: JobId,
        /// Also list the files.
        #[arg(long)]
        files: bool,
        /// Also list the connected peers (active torrents).
        #[arg(long)]
        peers: bool,
    },
    /// Rename a download (waiting/paused: new output name; complete: move on disk).
    Rename {
        /// Job ID.
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
    Cleanup {
        /// Job ID.
        #[arg(add = comp::job_ids_finished())]
        id: JobId,
        /// Do not ask for confirmation.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Daemon configuration.
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Show the formats, subtitles and quality plans of a video URL (yt-dlp).
    Probe {
        /// Video page or stream URL.
        url: String,
        /// List the playlist entries instead of probing one video.
        #[arg(long)]
        playlist: bool,
        /// Read cookies from this browser (firefox, chrome, ...).
        #[arg(long, value_name = "BROWSER", add = comp::browser())]
        cookies_from_browser: Option<String>,
    },
    /// Show the files of a magnet link without downloading it.
    Preview {
        /// Magnet link.
        magnet: String,
        /// Give up after this many seconds (default 120).
        #[arg(long, value_name = "SECS")]
        timeout: Option<u32>,
    },
    /// Pause downloads.
    Pause(TargetArgs),
    /// Resume downloads.
    Resume(TargetArgs),
    /// Remove downloads (files are kept unless --delete-files).
    Remove(RemoveArgs),
    /// Drop finished jobs from the history.
    Purge(PurgeArgs),
    /// Change a job's queue position.
    Reorder(ReorderArgs),
    /// Show or set the global speed limits.
    Limit(LimitArgs),
    /// Show or set the number of concurrent downloads.
    MaxConcurrent {
        /// New limit; omit to print the current one.
        n: Option<u32>,
    },
    /// Show the schedule windows, the active one and the next change.
    Schedule,
    /// Show global statistics.
    Stats,
    /// Print a shell completion script, or install it (`completions install`).
    #[command(after_help = "\
Examples:
  dman completions install                install for the shell in $SHELL
  dman completions install --shell fish   choose the shell explicitly
  dman completions bash > dman.bash       print the script (for package managers)")]
    Completions(CompletionsArgs),
    /// Search the enabled providers; optionally add results.
    #[command(after_help = "\
Examples:
  dman search ubuntu 24.04
  dman search 'blade runner' --provider YTS --category movies
  dman search ubuntu --limit 5 --add 1,2   add the first two results
  dman search --history")]
    Search(Box<SearchArgs>),
    /// Manage search providers (enable, disable, test, import, export).
    Providers {
        #[command(subcommand)]
        cmd: ProvidersCmd,
    },
    /// Public tracker list status.
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
    Serve(ServeArgs),
    /// Update dman (installer installs only) or show the right command for your install.
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
    /// Do not ask: run the package manager / restart the daemon.
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
    List,
    /// Enable providers (nothing is searched until one is enabled).
    Enable {
        /// Provider names.
        #[arg(required = true, add = comp::providers_disabled())]
        names: Vec<String>,
    },
    /// Disable providers.
    Disable {
        /// Provider names.
        #[arg(required = true, add = comp::providers_enabled())]
        names: Vec<String>,
    },
    /// Probe a provider and show what it returns.
    Test {
        /// Provider name.
        #[arg(add = comp::providers_any())]
        name: String,
    },
    /// Add or replace custom providers from a TOML file (`-` = stdin).
    Import {
        /// File of `[[provider]]` tables.
        #[arg(add = comp::any_file())]
        file: String,
    },
    /// Print providers as TOML (default: the custom ones).
    Export {
        /// Provider names.
        #[arg(add = comp::providers_any())]
        names: Vec<String>,
    },
    /// Remove a custom provider (built-ins can only be disabled).
    Remove {
        /// Provider name.
        #[arg(add = comp::providers_custom())]
        name: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum TrackersCmd {
    /// Show whether the tracker list is enabled, how many trackers are loaded and when.
    Status,
}

#[derive(Subcommand, Debug)]
pub enum DaemonCmd {
    /// Run the daemon in the foreground.
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
    Stop,
    /// Stop and start the daemon.
    Restart,
    /// Show daemon, engine and transfer status.
    Status,
    /// Start (or cleanly restart) the aria2 engine, e.g. after installing it.
    EngineRestart,
    /// Write the default config file if none exists.
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
    /// Print the access token (secret!) after starting. Never printed otherwise.
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
    /// Download speed limit for this job, e.g. 2MiB.
    #[arg(long, value_name = "SIZE", value_parser = parse_size)]
    pub max_download_limit: Option<u64>,
    /// Upload speed limit for this job, e.g. 500K.
    #[arg(long, value_name = "SIZE", value_parser = parse_size)]
    pub max_upload_limit: Option<u64>,
    /// Files to download (1-based), e.g. 1,3-5.
    #[arg(long, value_name = "LIST", value_parser = crate::commands::add::parse_select_files)]
    pub select_files: Option<Vec<u32>>,
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
    #[command(flatten)]
    pub video: VideoArgs,
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
    /// Keep running and redraw on every update (Ctrl-C to stop).
    #[arg(long)]
    pub watch: bool,
}

#[derive(Args, Debug)]
pub struct TargetArgs {
    /// Job IDs.
    #[arg(required_unless_present = "all", add = comp::job_ids_running())]
    pub ids: Vec<JobId>,
    /// Apply to every job.
    #[arg(long, conflicts_with = "ids")]
    pub all: bool,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCmd {
    /// Re-read config.toml in the running daemon and apply what can change live
    /// (limits, schedule, notifications, search, torrents, video defaults).
    Reload,
}

#[derive(Args, Debug)]
pub struct RemoveArgs {
    /// Job IDs.
    #[arg(required = true, add = comp::job_ids_any())]
    pub ids: Vec<JobId>,
    /// Also delete the downloaded files (asks a second time).
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
pub struct PurgeArgs {
    /// Job IDs; none = all stopped jobs.
    #[arg(add = comp::job_ids_stopped())]
    pub ids: Vec<JobId>,
    /// Do not ask for confirmation.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Args, Debug)]
#[command(group(clap::ArgGroup::new("move").required(true).multiple(false)))]
pub struct ReorderArgs {
    /// Job ID.
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
