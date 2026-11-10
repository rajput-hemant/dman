//! `dman providers ...` and `dman trackers status`.

use dman_proto::search::{ProviderInfo, ProviderSetParams};

use super::print_json;
use crate::cli::{ProvidersCmd, TrackersCmd};
use crate::connect::Ctx;
use crate::error::CliError;
use crate::format;

/// The text of a provider file (`-` = stdin).
fn read_source(file: &str) -> Result<String, CliError> {
    if file == "-" {
        let mut t = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut t)?;
        Ok(t)
    } else {
        std::fs::read_to_string(file)
            .map_err(|e| CliError::invalid(format!("cannot read {file}: {e}")))
    }
}

pub async fn run(ctx: &Ctx, cmd: &ProvidersCmd) -> Result<i32, CliError> {
    // A file that cannot be read is a usage error whether or not the daemon runs.
    let imported = match cmd {
        ProvidersCmd::Import { file } => Some(read_source(file)?),
        _ => None,
    };
    let client = ctx.connect().await?;
    match cmd {
        ProvidersCmd::List => {
            let list = client.providers_list().await?;
            if ctx.json {
                print_json(&list)?;
            } else {
                print!("{}", format::providers_table(&list));
                if list.iter().all(|p| !p.enabled) {
                    println!("\nno provider is enabled: `dman providers enable NAME`");
                }
            }
            Ok(0)
        }
        ProvidersCmd::Enable { names } => set_enabled(ctx, &client, names, true).await,
        ProvidersCmd::Disable { names } => set_enabled(ctx, &client, names, false).await,
        ProvidersCmd::Test { name } => {
            let r = client.providers_test(name).await?;
            if ctx.json {
                print_json(&r)?;
            } else if r.ok {
                println!(
                    "{}: ok, {} results in {} ms",
                    r.name, r.results, r.elapsed_ms
                );
                if let Some(m) = &r.mirror {
                    println!("answered by mirror: {m}");
                }
                if let Some(s) = &r.sample {
                    println!("sample: {}", s.title);
                }
                if r.results == 0 {
                    println!("(the provider answered but returned no results for its test query)");
                }
            } else {
                println!(
                    "{}: FAILED after {} ms: {}",
                    r.name,
                    r.elapsed_ms,
                    r.error.as_deref().unwrap_or("unknown error")
                );
            }
            Ok(i32::from(!r.ok))
        }
        ProvidersCmd::Import { .. } => {
            let text = imported.unwrap_or_default();
            let r = client.providers_import(&text).await?;
            if ctx.json {
                print_json(&r)?;
            } else {
                println!(
                    "imported {} (disabled; enable with `dman providers enable NAME`)",
                    r.names.join(", ")
                );
            }
            Ok(0)
        }
        ProvidersCmd::Export { names } => {
            let toml = client
                .providers_export((!names.is_empty()).then(|| names.clone()))
                .await?;
            if ctx.json {
                print_json(&toml)?;
            } else {
                print!("{toml}");
            }
            Ok(0)
        }
        ProvidersCmd::Remove { name } => {
            client.providers_remove(name).await?;
            if !ctx.json {
                println!("removed {name}");
            }
            Ok(0)
        }
    }
}

async fn set_enabled(
    ctx: &Ctx,
    client: &dman_client::Client,
    names: &[String],
    enabled: bool,
) -> Result<i32, CliError> {
    let mut done: Vec<ProviderInfo> = Vec::new();
    for name in names {
        let info = client
            .providers_set(ProviderSetParams {
                name: name.clone(),
                enabled: Some(enabled),
                ..ProviderSetParams::default()
            })
            .await?;
        if !ctx.json {
            println!(
                "{} {}",
                info.name,
                if enabled { "enabled" } else { "disabled" }
            );
            if enabled && !info.verified {
                eprintln!(
                    "note: {} has not been verified against the live site; check it with `dman providers test {}`",
                    info.name, info.name
                );
            }
        }
        done.push(info);
    }
    if ctx.json {
        print_json(&done)?;
    }
    Ok(0)
}

pub async fn trackers(ctx: &Ctx, cmd: &TrackersCmd) -> Result<i32, CliError> {
    match cmd {
        TrackersCmd::Status => {
            let st = ctx.connect().await?.trackers_status().await?;
            if ctx.json {
                print_json(&st)?;
            } else {
                let yn = |b: bool| if b { "yes" } else { "no" }.to_owned();
                let mut pairs = vec![
                    ("enabled", yn(st.enabled)),
                    ("attach to new", yn(st.attach_to_new)),
                    ("trackers", st.count.to_string()),
                    (
                        "last refresh",
                        st.last_refresh
                            .map_or_else(|| "never".into(), |t| format::date(Some(t))),
                    ),
                ];
                if let Some(e) = &st.last_error {
                    pairs.push(("last error", e.clone()));
                }
                print!("{}", format::key_values(&pairs));
                if !st.enabled {
                    println!(
                        "\nenable with `[search.tracker_list] enabled = true` in config.toml and restart the daemon"
                    );
                }
            }
            Ok(0)
        }
    }
}
