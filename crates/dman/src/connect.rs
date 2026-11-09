//! Profile resolution and daemon connections.

use dman_client::{Client, ClientError, TlsOptions};
use dman_config::expand_tilde;
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
        Self::load_profile(cli.profile.as_deref(), cli.json)
    }

    /// Like [`Ctx::load`] with the profile name given directly.
    pub fn load_profile(profile: Option<&str>, json: bool) -> Result<Self, CliError> {
        let paths = Paths::discover()?;
        let config = Config::load(&paths)?;
        config.validate()?;
        let (name, profile) = {
            let (n, p) = config.profile(profile)?;
            (n.to_owned(), p.clone())
        };
        Ok(Self {
            paths,
            config,
            name,
            profile,
            json,
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
        self.endpoint_with(true)
    }

    /// Like [`Ctx::endpoint`]; without `create_secret` a missing local secret is an error
    /// instead of being generated (shell completion must not write files).
    pub fn endpoint_with(&self, create_secret: bool) -> Result<(Url, SecretString), CliError> {
        let mut secrets = Secrets::load(&self.paths)?;
        match &self.profile.daemon {
            DaemonTarget::Local {
                listen,
                secret_ref,
                tls,
            } => {
                let scheme = if tls.is_some() { "wss" } else { "ws" };
                let url = parse_url(&format!("{scheme}://{listen}{WS_PATH}"))?;
                let token = if create_secret {
                    secrets.ensure(secret_ref)?
                } else {
                    secrets.get(secret_ref).ok_or_else(|| {
                        CliError::new(
                            dman_proto::ErrorCode::Unauthorized,
                            format!("secret '{secret_ref}' missing in secrets.toml"),
                        )
                    })?
                };
                Ok((url, token))
            }
            DaemonTarget::Remote {
                url, secret_ref, ..
            } => {
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

    /// TLS options for the profile's `wss://` connection: the configured CA, or
    /// `insecure_skip_verify` (warned about on every connect). A local daemon with
    /// `daemon.tls` is reached by its listen address, so its own certificate file is the
    /// trust anchor (works for a self-signed certificate whose SAN covers that address;
    /// with a public CA certificate use a remote profile with the host name instead).
    pub fn tls_options(&self) -> Result<TlsOptions, CliError> {
        match &self.profile.daemon {
            DaemonTarget::Local { tls: Some(t), .. } => TlsOptions::new()
                .with_ca_file(&t.cert_path())
                .map_err(CliError::from),
            DaemonTarget::Local { .. } => Ok(TlsOptions::new()),
            DaemonTarget::Remote {
                ca,
                insecure_skip_verify,
                ..
            } => {
                let mut o = TlsOptions::new().insecure_skip_verify(*insecure_skip_verify);
                if let Some(ca) = ca {
                    o = o.with_ca_file(&expand_tilde(ca))?;
                }
                Ok(o)
            }
        }
    }

    /// Connect to the daemon, or fail with a hint when none is running.
    pub async fn connect(&self) -> Result<Client, CliError> {
        let (url, token) = self.endpoint()?;
        Client::connect_tls(&url, token, CLIENT_NAME, &self.tls_options()?)
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

    fn ctx(daemon: DaemonTarget) -> (Ctx, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile {
            daemon,
            ..Profile::default()
        };
        let ctx = Ctx {
            paths: Paths::with_root(dir.path()),
            config: Config::default(),
            name: "work".into(),
            profile,
            json: false,
        };
        (ctx, dir)
    }

    fn remote(ca: Option<&str>, insecure: bool) -> DaemonTarget {
        DaemonTarget::Remote {
            url: "wss://nas.example:6810".into(),
            secret_ref: "work".into(),
            ca: ca.map(str::to_owned),
            insecure_skip_verify: insecure,
        }
    }

    #[test]
    fn remote_profile_options() {
        let (c, _d) = ctx(remote(None, true));
        assert!(c.tls_options().is_ok());
        let (c, _d) = ctx(remote(Some("/nonexistent/ca.pem"), false));
        assert!(c.tls_options().is_err(), "a missing CA file is an error");
    }

    #[test]
    fn remote_endpoint_defaults_ws_path_and_needs_secret() {
        let (c, d) = ctx(remote(None, false));
        assert_eq!(c.endpoint().err().unwrap().exit, 4);
        std::fs::write(d.path().join("secrets.toml"), "[secrets]\nwork = \"tok\"\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(
                d.path().join("secrets.toml"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        let (url, _) = c.endpoint().unwrap();
        assert_eq!(url.as_str(), "wss://nas.example:6810/api/v1/ws");
    }

    #[test]
    fn bad_url_is_usage_error() {
        assert_eq!(parse_url("not a url").unwrap_err().exit, 2);
    }
}
