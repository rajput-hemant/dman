//! `dman handle` (links handed over by the OS) and `dman integrate` (register
//! dman as the handler of magnet links and `.torrent` files).

use std::path::{Path, PathBuf};
use std::process::Command;

use dman_proto::{AddOutcome, AddParams, DuplicateInfo, JobOptions};

use crate::cli::{Cli, HandleArgs, IntegrateCmd, VideoArgs};
use crate::commands::{add, clipboard, daemon, print_json};
use crate::connect::Ctx;
use crate::error::CliError;

// ----- dman handle -------------------------------------------------------------

/// A source handed over by the OS: `file://` URLs (file managers) become paths.
pub fn handoff_input(s: &str) -> Result<String, CliError> {
    let t = s.trim();
    if t.get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("file://"))
    {
        let path = url::Url::parse(t)
            .ok()
            .and_then(|u| u.to_file_path().ok())
            .ok_or_else(|| CliError::invalid(format!("not a local file: {t}")))?;
        return Ok(path.to_string_lossy().into_owned());
    }
    Ok(t.to_owned())
}

/// Title and body of the desktop notification after a hand-off.
pub fn summary(labels: &[String], outcomes: &[AddOutcome]) -> (String, String) {
    let mut lines = Vec::new();
    let (mut added, mut known, mut failed) = (0, 0, 0);
    for (label, o) in labels.iter().zip(outcomes) {
        let shown = clipboard::shown(label);
        match o {
            AddOutcome::Added { .. } => {
                added += 1;
                lines.push(shown);
            }
            AddOutcome::Failed { error } if DuplicateInfo::from_error(error).is_some() => {
                known += 1;
                lines.push(format!("{shown}: {}", error.message));
            }
            AddOutcome::Failed { error } => {
                failed += 1;
                lines.push(format!("{shown}: {}", error.message));
            }
        }
    }
    let title = match (added, known, failed) {
        (_, 0, 0) => "Added to dman".to_owned(),
        (0, _, 0) => "Already in dman".to_owned(),
        (0, 0, _) => "dman could not add the link".to_owned(),
        _ => format!("dman: {added} added, {} not added", known + failed),
    };
    (title, lines.join("\n"))
}

/// Shows a desktop notification; quietly nothing without a notification
/// service or without the `notify` feature.
#[cfg(feature = "notify")]
async fn notify(title: String, body: String) {
    let _ = tokio::task::spawn_blocking(move || {
        if let Err(e) = notify_rust::Notification::new()
            .appname("dman")
            .summary(&title)
            .body(&body)
            .show()
        {
            tracing::debug!("desktop notification not shown: {e}");
        }
    })
    .await;
}

#[cfg(not(feature = "notify"))]
async fn notify(_title: String, _body: String) {}

async fn handle_inner(cli: &Cli, args: &HandleArgs) -> Result<i32, CliError> {
    let ctx = Ctx::load(cli)?;
    let inputs = args
        .sources
        .iter()
        .map(|s| handoff_input(s))
        .collect::<Result<Vec<_>, _>>()?;
    let (sources, labels, _) = add::build_sources(&inputs, false, &VideoArgs::default())?;
    let client = daemon::connect_or_start(&ctx).await?;
    let outcomes = client
        .add(AddParams {
            sources,
            options: JobOptions::default(),
            paused: false,
            force: false,
        })
        .await?;
    let (title, body) = summary(&labels, &outcomes);
    notify(title, body).await;
    add::report(&labels, &outcomes, ctx.json, true)
}

/// `dman handle <uri-or-path>...`
pub async fn handle(cli: &Cli, args: &HandleArgs) -> Result<i32, CliError> {
    let r = handle_inner(cli, args).await;
    if let Err(e) = &r
        && !e.message.is_empty()
    {
        notify("dman could not add the link".into(), e.message.clone()).await;
    }
    r
}

// ----- dman integrate ----------------------------------------------------------

/// MIME types the handler registers for.
pub const MIME_TYPES: [&str; 2] = ["x-scheme-handler/magnet", "application/x-bittorrent"];
/// The desktop entry `install` writes (a name no distribution package uses).
pub const DESKTOP_NAME: &str = "dman-handler.desktop";
/// The line that marks a desktop entry as written by dman.
const MARKER: &str = "X-Dman-Handler=true";

/// Quotes one argument for the `Exec` key of a desktop entry. Arguments with
/// reserved characters are double-quoted with `"`, `` ` ``, `$` and `\`
/// escaped; `%` is doubled. The result is a string value, so its own
/// backslashes are escaped once more.
pub fn exec_quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+,:@".contains(c));
    let quoted = if plain {
        arg.replace('%', "%%")
    } else {
        let mut q = String::from("\"");
        for c in arg.chars() {
            match c {
                '"' | '`' | '$' | '\\' => {
                    q.push('\\');
                    q.push(c);
                }
                '%' => q.push_str("%%"),
                c => q.push(c),
            }
        }
        q.push('"');
        q
    };
    quoted.replace('\\', "\\\\")
}

/// The desktop entry that opens magnet links and `.torrent` files with `exe handle`.
pub fn desktop_entry(exe: &Path) -> String {
    format!(
        "# Written by `dman integrate install`; `dman integrate uninstall` removes it.\n\
         [Desktop Entry]\n\
         Type=Application\n\
         Name=dman\n\
         GenericName=Download Manager\n\
         Comment=Add magnet links and .torrent files to dman\n\
         Exec={} handle %u\n\
         Terminal=false\n\
         NoDisplay=true\n\
         MimeType={};\n\
         Categories=Network;FileTransfer;P2P;\n\
         {MARKER}\n",
        exec_quote(&exe.to_string_lossy()),
        MIME_TYPES.join(";"),
    )
}

/// Whether a desktop entry was written by dman.
pub fn is_ours(content: &str) -> bool {
    content.lines().any(|l| l.trim() == MARKER)
}

/// `$XDG_DATA_HOME/applications`, else `~/.local/share/applications`.
fn applications_dir() -> Result<PathBuf, CliError> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|h| h.join(".local/share"))
        })
        .ok_or_else(|| CliError::general("cannot find the home directory (HOME is not set)"))?;
    Ok(data.join("applications"))
}

/// Writes the desktop entry into `dir`. An existing file is replaced only
/// when dman wrote it.
pub fn write_entry(dir: &Path, exe: &Path) -> Result<PathBuf, CliError> {
    let path = dir.join(DESKTOP_NAME);
    if let Ok(existing) = std::fs::read_to_string(&path)
        && !is_ours(&existing)
    {
        return Err(CliError::refused(format!(
            "{} exists and was not written by dman; left as it is",
            path.display()
        )));
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| CliError::general(format!("cannot create {}: {e}", dir.display())))?;
    std::fs::write(&path, desktop_entry(exe))
        .map_err(|e| CliError::general(format!("cannot write {}: {e}", path.display())))?;
    Ok(path)
}

/// What `uninstall` found.
#[derive(Debug, PartialEq, Eq)]
pub enum Removal {
    Removed(PathBuf),
    Missing(PathBuf),
}

/// Deletes the desktop entry in `dir`, but only when its content is dman's.
pub fn remove_entry(dir: &Path) -> Result<Removal, CliError> {
    let path = dir.join(DESKTOP_NAME);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Removal::Missing(path)),
        Err(e) => {
            return Err(CliError::general(format!(
                "cannot read {}: {e}",
                path.display()
            )));
        }
    };
    if !is_ours(&content) {
        return Err(CliError::refused(format!(
            "{} was not written by dman; left as it is",
            path.display()
        )));
    }
    std::fs::remove_file(&path)
        .map_err(|e| CliError::general(format!("cannot remove {}: {e}", path.display())))?;
    Ok(Removal::Removed(path))
}

/// Runs a helper program; `None` when it is not installed or failed.
fn tool(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// The desktop entry that opens `mime` (via `xdg-mime`).
fn xdg_default(mime: &str) -> Option<String> {
    tool("xdg-mime", &["query", "default", mime]).filter(|s| !s.is_empty())
}

fn this_exe() -> Result<PathBuf, CliError> {
    let exe = std::env::current_exe()
        .map_err(|e| CliError::general(format!("cannot find the dman executable: {e}")))?;
    Ok(std::fs::canonicalize(&exe).unwrap_or(exe))
}

fn linux(cmd: IntegrateCmd, json: bool) -> Result<i32, CliError> {
    let dir = applications_dir()?;
    let dir_s = dir.to_string_lossy().into_owned();
    match cmd {
        IntegrateCmd::Install => {
            let path = write_entry(&dir, &this_exe()?)?;
            let mut args = vec!["default", DESKTOP_NAME];
            args.extend(MIME_TYPES);
            let default_set = tool("xdg-mime", &args).is_some();
            let _ = tool("update-desktop-database", &[&dir_s]);
            if json {
                print_json(&serde_json::json!({
                    "status": "installed",
                    "file": path,
                    "default_set": default_set,
                }))?;
            } else {
                println!("wrote {}", path.display());
                if default_set {
                    println!("magnet links and .torrent files now open with `dman handle`");
                } else {
                    println!(
                        "xdg-mime is not available: choose \"dman\" for magnet links in your \
                         desktop's default-application settings"
                    );
                }
            }
            Ok(0)
        }
        IntegrateCmd::Uninstall => {
            let removal = remove_entry(&dir)?;
            let _ = tool("update-desktop-database", &[&dir_s]);
            let still_default = xdg_default(MIME_TYPES[0]).is_some_and(|d| d == DESKTOP_NAME);
            if json {
                let (status, file) = match &removal {
                    Removal::Removed(p) => ("removed", p),
                    Removal::Missing(p) => ("not_installed", p),
                };
                print_json(&serde_json::json!({ "status": status, "file": file }))?;
            } else {
                match &removal {
                    Removal::Removed(p) => println!("removed {}", p.display()),
                    Removal::Missing(p) => {
                        println!("nothing to remove ({} does not exist)", p.display());
                    }
                }
                if still_default {
                    println!(
                        "magnet links have no working handler now; choose one in your desktop \
                         settings or with `xdg-mime default <app>.desktop x-scheme-handler/magnet`"
                    );
                }
            }
            Ok(0)
        }
        IntegrateCmd::Status => {
            let path = dir.join(DESKTOP_NAME);
            let file = match std::fs::read_to_string(&path) {
                Ok(c) if is_ours(&c) => "installed",
                Ok(_) => "present, not written by dman",
                Err(_) => "not installed",
            };
            let magnet = xdg_default(MIME_TYPES[0]);
            let torrent = xdg_default(MIME_TYPES[1]);
            if json {
                print_json(&serde_json::json!({
                    "file": path,
                    "installed": file == "installed",
                    "magnet_handler": magnet,
                    "torrent_handler": torrent,
                }))?;
            } else {
                let unknown = || "unknown (xdg-mime is not available or has no default)".to_owned();
                print!(
                    "{}",
                    crate::format::key_values(&[
                        ("handler file", format!("{} ({file})", path.display())),
                        ("magnet links", magnet.unwrap_or_else(unknown)),
                        (".torrent files", torrent.unwrap_or_else(unknown)),
                    ])
                );
            }
            Ok(0)
        }
    }
}

/// Why macOS needs more than a command.
pub const MACOS_HELP: &str = "\
macOS opens URL schemes only through application bundles, so dman cannot register
itself from the command line. To hand magnet links to dman, make a tiny app:

  1. Open Script Editor, paste the following and save it as an Application
     named \"dman handler\" (File > Export > File Format: Application):

       on open location theURL
         do shell script \"DMAN handle \" & quoted form of theURL
       end open location
       on open theFiles
         repeat with f in theFiles
           do shell script \"DMAN handle \" & quoted form of POSIX path of f
         end repeat
       end open

  2. In the app's Contents/Info.plist add CFBundleURLTypes with the URL scheme
     `magnet`, and CFBundleDocumentTypes for the extension `torrent`.
  3. Open the app once, then make it the default, e.g. with `duti`:
       duti -s <bundle id> magnet
       duti -s <bundle id> .torrent all

See docs/browser.md for details and for the browser bookmarklet.";

fn macos(cmd: IntegrateCmd, json: bool) -> Result<i32, CliError> {
    let exe = this_exe()?;
    let help = MACOS_HELP.replace("DMAN", &exe.to_string_lossy());
    match cmd {
        IntegrateCmd::Install => {
            if json {
                print_json(&serde_json::json!({ "status": "manual", "instructions": help }))?;
            } else {
                println!("{help}");
            }
            Ok(dman_proto::ErrorCode::Refused.exit_code())
        }
        IntegrateCmd::Uninstall | IntegrateCmd::Status => {
            let text = "dman registers nothing on macOS; a handler app made as described by \
                        `dman integrate install` is removed by deleting that app";
            say(json, text, "not_installed")?;
            Ok(0)
        }
    }
}

/// One status line: `{"status": ...}` with `--json`, else the text.
fn say(json: bool, human: &str, status: &str) -> Result<(), CliError> {
    if json {
        print_json(&serde_json::json!({ "status": status }))
    } else {
        println!("{human}");
        Ok(())
    }
}

/// Registry keys under `HKEY_CURRENT_USER` that `install` writes on Windows.
const WIN_MAGNET: &str = r"HKCU\Software\Classes\magnet";
const WIN_PROGID: &str = r"HKCU\Software\Classes\dman.torrent";
const WIN_EXT: &str = r"HKCU\Software\Classes\.torrent";

/// The command line Windows runs for a magnet link or `.torrent` file.
pub fn windows_open_command(exe: &Path) -> String {
    format!("\"{}\" handle \"%1\"", exe.display())
}

/// The `reg.exe` invocations of `install`.
pub fn windows_install_commands(exe: &Path) -> Vec<Vec<String>> {
    let open = windows_open_command(exe);
    let cmd = |key: &str| format!(r"{key}\shell\open\command");
    let add = |key: String, value: Option<&str>, data: &str| {
        let mut v = vec!["add".to_owned(), key];
        match value {
            Some(name) => v.extend(["/v".to_owned(), name.to_owned()]),
            None => v.push("/ve".to_owned()),
        }
        v.extend(["/d".to_owned(), data.to_owned(), "/f".to_owned()]);
        v
    };
    vec![
        add(WIN_MAGNET.to_owned(), None, "URL:Magnet link"),
        add(WIN_MAGNET.to_owned(), Some("URL Protocol"), ""),
        add(cmd(WIN_MAGNET), None, &open),
        add(WIN_PROGID.to_owned(), None, "Torrent file (dman)"),
        add(cmd(WIN_PROGID), None, &open),
        add(WIN_EXT.to_owned(), None, "dman.torrent"),
    ]
}

/// Whether a registered open command is dman's (`... dman... handle "%1"`).
pub fn is_our_windows_command(query_output: &str) -> bool {
    let lower = query_output.to_ascii_lowercase();
    lower.contains("dman") && lower.contains(" handle \"%1\"")
}

fn reg(args: &[&str]) -> Option<String> {
    tool("reg", args)
}

fn windows(cmd: IntegrateCmd, json: bool) -> Result<i32, CliError> {
    let magnet_cmd = format!(r"{WIN_MAGNET}\shell\open\command");
    match cmd {
        IntegrateCmd::Install => {
            for args in windows_install_commands(&this_exe()?) {
                let a: Vec<&str> = args.iter().map(String::as_str).collect();
                if reg(&a).is_none() {
                    return Err(CliError::general(format!(
                        "reg.exe failed: reg {}",
                        args.join(" ")
                    )));
                }
            }
            say(
                json,
                "registered dman for magnet links and .torrent files (HKEY_CURRENT_USER)",
                "installed",
            )?;
            Ok(0)
        }
        IntegrateCmd::Uninstall => {
            let mut removed = Vec::new();
            if reg(&["query", &magnet_cmd, "/ve"]).is_some_and(|o| is_our_windows_command(&o))
                && reg(&["delete", WIN_MAGNET, "/f"]).is_some()
            {
                removed.push(WIN_MAGNET);
            }
            let progid_cmd = format!(r"{WIN_PROGID}\shell\open\command");
            if reg(&["query", &progid_cmd, "/ve"]).is_some_and(|o| is_our_windows_command(&o))
                && reg(&["delete", WIN_PROGID, "/f"]).is_some()
            {
                removed.push(WIN_PROGID);
            }
            if reg(&["query", WIN_EXT, "/ve"]).is_some_and(|o| o.contains("dman.torrent"))
                && reg(&["delete", WIN_EXT, "/ve", "/f"]).is_some()
            {
                removed.push(WIN_EXT);
            }
            if json {
                print_json(&serde_json::json!({ "status": "removed", "keys": removed }))?;
            } else if removed.is_empty() {
                println!("nothing to remove (no registry entries of dman found)");
            } else {
                println!("removed {}", removed.join(", "));
            }
            Ok(0)
        }
        IntegrateCmd::Status => {
            let current = reg(&["query", &magnet_cmd, "/ve"]);
            let ours = current.as_deref().is_some_and(is_our_windows_command);
            if json {
                print_json(&serde_json::json!({
                    "installed": ours,
                    "magnet_handler": current,
                }))?;
            } else {
                let handler = current.map_or_else(
                    || "none registered for this user".to_owned(),
                    |c| {
                        c.lines()
                            .find(|l| l.contains("REG_"))
                            .map_or(c.clone(), |l| l.trim().to_owned())
                    },
                );
                print!(
                    "{}",
                    crate::format::key_values(&[
                        (
                            "dman registered",
                            if ours { "yes" } else { "no" }.to_owned()
                        ),
                        ("magnet links", handler),
                    ])
                );
            }
            Ok(0)
        }
    }
}

/// `dman integrate install|uninstall|status`.
pub fn run(cmd: IntegrateCmd, json: bool) -> Result<i32, CliError> {
    if cfg!(windows) {
        windows(cmd, json)
    } else if cfg!(target_os = "macos") {
        macos(cmd, json)
    } else {
        linux(cmd, json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dman_proto::{JobId, JobState, RpcError};

    #[test]
    fn file_urls_become_paths() {
        assert_eq!(
            handoff_input("file:///home/me/a%20b.torrent").unwrap(),
            "/home/me/a b.torrent"
        );
        assert_eq!(handoff_input(" magnet:?xt=x ").unwrap(), "magnet:?xt=x");
        assert_eq!(handoff_input("/tmp/a.torrent").unwrap(), "/tmp/a.torrent");
        assert_eq!(handoff_input("file://host/x").unwrap_err().exit, 2);
    }

    #[test]
    fn exec_arguments_are_quoted() {
        assert_eq!(exec_quote("/usr/bin/dman"), "/usr/bin/dman");
        assert_eq!(
            exec_quote("/home/me/my apps/dman"),
            "\"/home/me/my apps/dman\""
        );
        // `$` and `"` are escaped for the Exec parser, then the backslash
        // itself for the string value.
        assert_eq!(exec_quote("/a$b\"c"), "\"/a\\\\$b\\\\\"c\"");
        assert_eq!(exec_quote("/a%b"), "\"/a%%b\"");
    }

    #[test]
    fn desktop_entry_registers_both_types_and_is_recognised() {
        let e = desktop_entry(Path::new("/usr/bin/dman"));
        assert!(e.contains("Exec=/usr/bin/dman handle %u\n"), "{e}");
        assert!(e.contains("MimeType=x-scheme-handler/magnet;application/x-bittorrent;\n"));
        assert!(is_ours(&e));
        assert!(!is_ours("[Desktop Entry]\nName=Other\n"));
    }

    #[test]
    fn install_and_uninstall_touch_only_our_file() {
        let d = tempfile::tempdir().unwrap();
        let apps = d.path().join("applications");
        assert_eq!(
            remove_entry(&apps).unwrap(),
            Removal::Missing(apps.join(DESKTOP_NAME))
        );
        let p = write_entry(&apps, Path::new("/x/dman")).unwrap();
        assert!(
            std::fs::read_to_string(&p)
                .unwrap()
                .contains("/x/dman handle")
        );
        // Re-installing replaces our own file.
        write_entry(&apps, Path::new("/y/dman")).unwrap();
        assert!(
            std::fs::read_to_string(&p)
                .unwrap()
                .contains("/y/dman handle")
        );
        assert_eq!(remove_entry(&apps).unwrap(), Removal::Removed(p.clone()));
        assert!(!p.exists());

        // A file of the same name that dman did not write is never touched.
        std::fs::write(&p, "[Desktop Entry]\nName=Mine\n").unwrap();
        assert_eq!(
            write_entry(&apps, Path::new("/x/dman")).unwrap_err().exit,
            6
        );
        assert_eq!(remove_entry(&apps).unwrap_err().exit, 6);
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "[Desktop Entry]\nName=Mine\n"
        );
    }

    #[test]
    fn windows_registry_entries() {
        let exe = Path::new(r"C:\Tools\dman.exe");
        assert_eq!(
            windows_open_command(exe),
            r#""C:\Tools\dman.exe" handle "%1""#
        );
        let cmds = windows_install_commands(exe);
        assert!(
            cmds.iter()
                .all(|c| c[0] == "add" && c.last().unwrap() == "/f")
        );
        assert!(
            cmds.iter()
                .all(|c| c[1].starts_with(r"HKCU\Software\Classes\"))
        );
        assert!(cmds.iter().any(|c| c.contains(&"URL Protocol".to_owned())));
        assert!(is_our_windows_command(
            r#"    (Default)    REG_SZ    "C:\Tools\dman.exe" handle "%1""#
        ));
        assert!(!is_our_windows_command(
            r#"    (Default)    REG_SZ    "C:\qBittorrent\qbittorrent.exe" "%1""#
        ));
    }

    #[test]
    fn notification_summaries() {
        let labels = vec!["magnet:?xt=urn:btih:a&dn=Ubuntu".to_owned()];
        let ok = vec![AddOutcome::Added {
            id: JobId(3),
            extra_ids: vec![],
        }];
        assert_eq!(
            summary(&labels, &ok),
            ("Added to dman".into(), "magnet link \"Ubuntu\"".into())
        );
        let mut e = RpcError::refused("already in your downloads as job 2 (paused)");
        e.data = Some(
            DuplicateInfo {
                id: JobId(2),
                state: JobState::Paused,
                trackers_added: 0,
            }
            .to_data(),
        );
        let dup = vec![AddOutcome::Failed { error: e }];
        let (t, b) = summary(&labels, &dup);
        assert_eq!(t, "Already in dman");
        assert!(b.contains("job 2 (paused)"), "{b}");
        let bad = vec![AddOutcome::Failed {
            error: RpcError::invalid("aria2 rejected the source"),
        }];
        assert_eq!(summary(&labels, &bad).0, "dman could not add the link");
        assert!(MACOS_HELP.contains("open location"));
    }
}
