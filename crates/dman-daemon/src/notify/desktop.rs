//! Desktop notification on the daemon host. Fails quietly: a headless server
//! has no notification service, which is the normal case.

use super::Payload;

/// Shows a notification from a blocking thread.
pub(super) fn show(p: &Payload) {
    let (summary, body) = match p.event.as_str() {
        "failed" => (
            "Download failed",
            format!("{}\n{}", p.name, p.error.as_deref().unwrap_or("")),
        ),
        "seed_completed" => ("Seeding finished", p.name.clone()),
        _ => ("Download complete", p.name.clone()),
    };
    let body = body.trim_end().to_owned();
    tokio::task::spawn_blocking(move || {
        let r = notify_rust::Notification::new()
            .appname("dman")
            .summary(summary)
            .body(&body)
            .show();
        if let Err(e) = r {
            tracing::debug!("desktop notification not shown: {e}");
        }
    });
}
