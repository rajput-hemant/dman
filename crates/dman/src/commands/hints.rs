//! Did-you-mean suggestions for "not found" answers of the daemon (job ids and
//! provider names), added after the command failed so every command gets them.

use std::time::Duration;

use dman_client::Client;
use dman_proto::ErrorCode;
use dman_proto::method::ListParams;

use crate::connect::Ctx;
use crate::error::CliError;
use crate::suggest;

/// Longest wait for the extra lookup; the original error is reported without
/// suggestions when the daemon does not answer in time.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);

/// What a "not found" message names.
#[derive(Debug, PartialEq, Eq)]
enum Missing {
    Job(String),
    Provider(String),
}

/// Recognise `job 12 not found` and `unknown provider `x``/`provider `x` not found`.
fn missing_from(message: &str) -> Option<Missing> {
    if let Some(id) = message
        .strip_prefix("job ")
        .and_then(|r| r.strip_suffix(" not found"))
    {
        return Some(Missing::Job(id.trim().to_owned()));
    }
    let quoted = |s: &str| {
        let start = s.find('`')? + 1;
        let end = start + s[start..].find('`')?;
        Some(s[start..end].to_owned())
    };
    if message.starts_with("unknown provider") || message.contains("provider `") {
        return quoted(message).map(Missing::Provider);
    }
    None
}

/// Adds "did you mean ...?" to a not-found error when the daemon can tell what
/// exists. Other errors, and failures of the lookup itself, pass through.
pub async fn enrich(profile: Option<&str>, json: bool, err: CliError) -> CliError {
    if err.code != ErrorCode::NotFound {
        return err;
    }
    let Some(missing) = missing_from(&err.message) else {
        return err;
    };
    let lookup = async {
        let ctx = Ctx::load_profile(profile, json).ok()?;
        let client = ctx.connect().await.ok()?;
        suggestions(&client, &missing).await
    };
    match tokio::time::timeout(LOOKUP_TIMEOUT, lookup).await {
        Ok(Some(extra)) => {
            let base = err.hint.clone().unwrap_or_default();
            let mut e = err;
            e.hint = Some(if base.is_empty() {
                extra
            } else {
                format!("{extra} ({base})")
            });
            e
        }
        _ => err,
    }
}

async fn suggestions(client: &Client, missing: &Missing) -> Option<String> {
    match missing {
        Missing::Job(id) => {
            let jobs = client.list(ListParams::default()).await.ok()?;
            let ids: Vec<String> = jobs.iter().map(|j| j.id.to_string()).collect();
            let close = suggest::closest(id, ids.iter().map(String::as_str), 3);
            let named: Vec<String> = close
                .iter()
                .map(|c| {
                    jobs.iter().find(|j| j.id.to_string() == *c).map_or_else(
                        || c.clone(),
                        |j| format!("{c} ({})", crate::format::printable(&j.name)),
                    )
                })
                .collect();
            suggest::did_you_mean(&named)
        }
        Missing::Provider(name) => {
            let list = client.providers_list().await.ok()?;
            let close = suggest::closest(name, list.iter().map(|p| p.name.as_str()), 3);
            suggest::did_you_mean(&close)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_what_is_missing() {
        assert_eq!(
            missing_from("job 12 not found"),
            Some(Missing::Job("12".into()))
        );
        assert_eq!(
            missing_from("unknown provider `nyaaa`"),
            Some(Missing::Provider("nyaaa".into()))
        );
        assert_eq!(
            missing_from("provider `yts` not found"),
            Some(Missing::Provider("yts".into()))
        );
        assert_eq!(missing_from("file not found"), None);
    }
}
