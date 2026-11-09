//! Profile resolution and daemon connections.

use dman_client::{Client, ClientError};
use dman_config::{Config, DaemonTarget, Paths, Profile, Secrets};
use secrecy::SecretString;
use url::Url;

use crate::cli::Cli;
use crate::error::CliError;

/// Client name sent in `session.hello`.
pub const CLIENT_NAME: &str = concat!("dman-cli/", env!("CARGO_PKG_VERSION"));

const WS_PATH: &str = "/api/v1/ws";

/// Everything a command needs to know about the selected profile.
pub struct Ctx {
    pub paths: Paths,
    pub config: Config,
    pub name: String,
    pub profile: Profile,
    pub json: bool,
}

impl Ctx {
    /// Load and validate the configuration and select the profile.
    pub fn load(cli: &Cli) -> Result<Self, CliError> {
        let paths = Paths::discover()?;
        let config = Config::load(&paths)?;
        config.validate()?;
        let (name, profile) = {
            let (n, p) = config.profile(cli.profile.as_deref())?;
            (n.to_owned(), p.clone())
        };
        Ok(Self {
            paths,
            config,
            name,
            profile,
            json: cli.json,
        })
    }

    pub fn is_local(&self) -> bool {
        matches!(self.profile.daemon, DaemonTarget::Local { .. })
    }

    /// Fail with a usage error unless the profile is local.
    pub fn require_local(&self, what: &str) -> Result<(), CliError> {
        if self.is_local() {
            Ok(())
        } else {
            Err(CliError::invalid(format!(
                "{what} only works for local profiles (profile '{}' is remote)",
                self.name
            )))
        }
    }

    /// WebSocket URL and API token of the profile's daemon.
    pub fn endpoint(&self) -> Result<(Url, SecretString), CliError> {
        let mut secrets = Secrets::load(&self.paths)?;
        match &self.profile.daemon {
            DaemonTarget::Local { listen, secret_ref } => {
                let url = parse_url(&format!("ws://{listen}{WS_PATH}"))?;
                Ok((url, secrets.ensure(secret_ref)?))
            }
            DaemonTarget::Remote { url, secret_ref } => {
                let mut url = parse_url(url)?;
                if matches!(url.path(), "" | "/") {
                    url.set_path(WS_PATH);
                }
                let token = secrets.get(secret_ref).ok_or_else(|| {
                    CliError::new(
                        dman_proto::ErrorCode::Unauthorized,
                        format!("secret '{secret_ref}' missing in secrets.toml"),
                    )
                })?;
                Ok((url, token))
            }
        }
    }

    /// Connect to the daemon, or fail with a hint when none is running.
    pub async fn connect(&self) -> Result<Client, CliError> {
        let (url, token) = self.endpoint()?;
        Client::connect(&url, token, CLIENT_NAME)
            .await
            .map_err(|e| self.connect_error(e, &url))
    }

    /// Whether `e` means "nothing is listening" (as opposed to a bad token etc.).
    pub fn is_not_running(e: &CliError) -> bool {
        e.code == dman_proto::ErrorCode::Unreachable
    }

    fn connect_error(&self, e: ClientError, url: &Url) -> CliError {
        match e {
            ClientError::Connect(detail) if self.is_local() => {
                tracing::debug!("connect failed: {detail}");
                CliError::unreachable("daemon not running (start it with `dman daemon start`)")
            }
            ClientError::Connect(detail) => {
                CliError::unreachable(format!("cannot connect to daemon at {url}: {detail}"))
            }
            other => other.into(),
        }
    }
}

fn parse_url(s: &str) -> Result<Url, CliError> {
    Url::parse(s).map_err(|e| CliError::invalid(format!("invalid daemon URL '{s}': {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_url_is_usage_error() {
        assert_eq!(parse_url("not a url").unwrap_err().exit, 2);
    }
}
