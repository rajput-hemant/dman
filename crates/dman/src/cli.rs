//! Command-line definition (clap derive).

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use dman_proto::{JobId, JobState};

/// dman: a terminal-first download manager.
#[derive(Parser, Debug)]
#[command(name = "dman", version, propagate_version = true)]
pub struct Cli {
    /// Daemon profile to use.
    #[arg(long, global = true, env = "DMAN_PROFILE", value_name = "NAME")]
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

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Manage the background daemon.
    Daemon {
        #[command(subcommand)]
        cmd: DaemonCmd,
    },
    /// Add downloads (URLs, magnets, .torrent/.metalink files).
    Add(Box<AddArgs>),
    /// List downloads.
    List(ListArgs),
    /// Show details of one download.
    Get {
        /// Job ID.
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
        id: JobId,
        /// New file name (no `/`); for a completed multi-file torrent without
        /// --file, the new top-level directory name.
        name: String,
        /// File of a multi-file download (1-based).
        #[arg(long, value_name = "N")]
        file: Option<u32>,
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
    /// Print a shell completion script.
    Completions {
        /// Target shell.
        shell: Shell,
    },
    /// Search the enabled providers; optionally add results.
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
    /// Run the web UI (not available yet).
    Serve,
}

#[derive(Args, Debug, Default)]
pub struct SearchArgs {
    /// Query words.
    #[arg(required_unless_present_any = ["history", "clear_history"])]
    pub query: Vec<String>,
    /// Only this provider (repeatable); default: every enabled provider.
    #[arg(short, long = "provider", value_name = "NAME")]
    pub provider: Vec<String>,
    /// Category filter (movies, tv, audio, software, books, xxx, other).
    #[arg(long, value_name = "CATEGORY")]
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
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Disable providers.
    Disable {
        /// Provider names.
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Probe a provider and show what it returns.
    Test {
        /// Provider name.
        name: String,
    },
    /// Add or replace custom providers from a TOML file (`-` = stdin).
    Import {
        /// File of `[[provider]]` tables.
        file: String,
    },
    /// Print providers as TOML (default: the custom ones).
    Export {
        /// Provider names.
        names: Vec<String>,
    },
    /// Remove a custom provider (built-ins can only be disabled).
    Remove {
        /// Provider name.
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
pub struct AddArgs {
    /// URLs, magnets, local .torrent/.metalink/.meta4 files, or `-` for stdin.
    pub sources: Vec<String>,
    /// Read sources from a text file (one per line; blank lines and # comments skipped).
    #[arg(long, value_name = "FILE")]
    pub input_file: Option<String>,
    /// Treat all URI sources as mirrors of ONE download.
    #[arg(long)]
    pub mirror: bool,
    /// Download directory (on the daemon host).
    #[arg(long, value_name = "DIR")]
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
    #[arg(required_unless_present = "all")]
    pub ids: Vec<JobId>,
    /// Apply to every job.
    #[arg(long, conflicts_with = "ids")]
    pub all: bool,
}

#[derive(Args, Debug)]
pub struct RemoveArgs {
    /// Job IDs.
    #[arg(required = true)]
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
    pub ids: Vec<JobId>,
    /// Do not ask for confirmation.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Args, Debug)]
#[command(group(clap::ArgGroup::new("move").required(true).multiple(false)))]
pub struct ReorderArgs {
    /// Job ID.
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
}
