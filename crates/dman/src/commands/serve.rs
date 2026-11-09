//! `dman serve`: the browser UI (decision D3).

use dman_config::{ConfigDoc, Secrets, Web};
use dman_proto::method::{WebStartParams, WebStatus};

use super::print_json;
use crate::cli::ServeArgs;
use crate::connect::Ctx;
use crate::error::CliError;

pub async fn run(ctx: &Ctx, args: &ServeArgs) -> Result<i32, CliError> {
    if args.stop {
        return stop(ctx, args).await;
    }
    if args.print_token {
        ctx.require_local("`serve --print-token`")?;
    }
    let mut web = ctx.config.web.clone();
    if let Some(b) = &args.bind {
        web.bind.clone_from(b);
    }
    if let Some(p) = args.port {
        web.port = p;
    }
    if let Some(b) = &args.base_path {
        web.base_path.clone_from(b);
    }
    web.enabled = true;
    web.validate().map_err(CliError::invalid)?;
    if ctx.is_local() {
        ensure_token(ctx, &web)?;
    }

    match ctx.connect().await {
        Ok(client) => {
            let status = client
                .web_start(WebStartParams {
                    bind: args.bind.clone(),
                    port: args.port,
                    base_path: args.base_path.clone(),
                    persist: args.persist,
                })
                .await?;
            report(ctx, &status, &web, args)?;
            Ok(0)
        }
        Err(e) if Ctx::is_not_running(&e) && ctx.is_local() => foreground(ctx, web, args).await,
        Err(e) => Err(e),
    }
}

/// Makes sure the access token exists when the UI asks for one: a network bind
/// (unless unauthenticated access was configured) or `require_login`. Loopback
/// without `require_login` needs no token (decision U3).
fn ensure_token(ctx: &Ctx, web: &Web) -> Result<(), CliError> {
    if web.loopback_without_login() || (web.allow_unauthenticated && !web.require_login) {
        return Ok(());
    }
    let mut secrets = Secrets::load(&ctx.paths)?;
    if secrets.get(&web.token_ref).is_none() {
        secrets.ensure(&web.token_ref)?;
    }
    Ok(())
}

async fn stop(ctx: &Ctx, args: &ServeArgs) -> Result<i32, CliError> {
    let client = match ctx.connect().await {
        Ok(c) => c,
        Err(e) if Ctx::is_not_running(&e) => {
            say(ctx, "daemon not running", "not_running")?;
            return Ok(0);
        }
        Err(e) => return Err(e),
    };
    client.web_stop(args.persist).await?;
    say(ctx, "web UI stopped", "stopped")?;
    Ok(0)
}

fn say(ctx: &Ctx, human: &str, status: &str) -> Result<(), CliError> {
    if ctx.json {
        print_json(&serde_json::json!({ "status": status }))?;
    } else {
        println!("{human}");
    }
    Ok(())
}

/// No daemon is running: run one in this process with the web UI on.
async fn foreground(ctx: &Ctx, web: Web, args: &ServeArgs) -> Result<i32, CliError> {
    if args.persist {
        persist(ctx, &web)?;
    }
    let status = WebStatus {
        enabled: true,
        url: Some(web.url()),
        exposed: !web.is_loopback(),
        auth: !web.loopback_without_login()
            && Secrets::load(&ctx.paths)?.get(&web.token_ref).is_some(),
    };
    report(ctx, &status, &web, args)?;
    let mut config = ctx.config.clone();
    config.web = web;
    let opts = dman_daemon::DaemonOptions {
        paths: ctx.paths.clone(),
        config,
        profile: ctx.name.clone(),
    };
    dman_daemon::run(opts)
        .await
        .map_err(|e| CliError::general(format!("{e:#}")))?;
    Ok(0)
}

fn persist(ctx: &Ctx, web: &Web) -> Result<(), CliError> {
    let mut doc = ConfigDoc::open(&ctx.paths)?;
    doc.set(&["web", "enabled"], true)?;
    doc.set(&["web", "bind"], web.bind.as_str())?;
    doc.set(&["web", "port"], i64::from(web.port))?;
    doc.set(&["web", "base_path"], web.base_path.as_str())?;
    doc.save()?;
    Ok(())
}

/// Prints the URL. Loopback without login: just the URL. A network bind: where the
/// token lives; the token itself only with `--print-token`.
fn report(ctx: &Ctx, s: &WebStatus, web: &Web, args: &ServeArgs) -> Result<(), CliError> {
    let url = s.url.clone().unwrap_or_else(|| web.url());
    let token = if args.print_token && s.auth {
        Secrets::load(&ctx.paths)?
            .get(&web.token_ref)
            .map(|t| secrecy::ExposeSecret::expose_secret(&t).to_owned())
    } else {
        None
    };
    if ctx.json {
        let mut v = serde_json::json!({
            "url": url, "exposed": s.exposed, "auth": s.auth,
        });
        if let Some(t) = &token {
            v["token"] = t.clone().into();
        }
        return print_json(&v);
    }
    if !s.auth && !s.exposed {
        println!("Web UI: {url} \u{2014} open it in your browser");
        if args.print_token {
            println!("(no token needed: the web UI only listens on this computer)");
        }
        return Ok(());
    }
    println!("Web UI: {url}");
    if s.auth {
        println!(
            "Access token: entry `{}` in {} (show it with `dman serve --print-token`)",
            web.token_ref,
            ctx.paths.secrets_file().display()
        );
    } else {
        println!("WARNING: no access token; anyone who can reach this address has full control");
    }
    if s.exposed {
        println!("WARNING: listening beyond loopback; use a firewall or a TLS reverse proxy");
    }
    if let Some(t) = token {
        println!("token: {t}");
    }
    Ok(())
}
