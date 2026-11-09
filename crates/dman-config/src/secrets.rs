//! `secrets.toml` handling (decision D21).

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use crate::{ConfigError, Paths, fsutil};

#[derive(Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SecretsFile {
    secrets: BTreeMap<String, String>,
}

/// Named secrets loaded from `secrets.toml` (`[secrets] name = "value"`).
/// `Debug` lists names only, never values.
#[derive(Clone)]
pub struct Secrets {
    path: PathBuf,
    map: BTreeMap<String, SecretString>,
}

impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Secrets")
            .field("path", &self.path)
            .field("names", &self.map.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Secrets {
    /// Load secrets. A missing file yields an empty set. On Unix the file is
    /// refused if group or others have any access (`mode & 0o077 != 0`).
    pub fn load(paths: &Paths) -> Result<Self, ConfigError> {
        let path = paths.secrets_file();
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    path,
                    map: BTreeMap::new(),
                });
            }
            Err(e) => return Err(ConfigError::io(path, e)),
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = meta.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                return Err(ConfigError::InsecureSecrets { path, mode });
            }
        }
        #[cfg(not(unix))]
        let _ = meta;
        let text = std::fs::read_to_string(&path).map_err(|e| ConfigError::io(&path, e))?;
        // Deliberately do not include the parser's source excerpt: it could echo secrets.
        let file: SecretsFile = toml::from_str(&text).map_err(|e| ConfigError::Parse {
            path: path.clone(),
            message: e.message().to_owned(),
            span: None,
        })?;
        let map = file
            .secrets
            .into_iter()
            .map(|(k, v)| (k, SecretString::from(v)))
            .collect();
        Ok(Self { path, map })
    }

    /// Look up a secret by name.
    pub fn get(&self, name: &str) -> Option<SecretString> {
        self.map.get(name).cloned()
    }

    /// Names of all stored secrets.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.map.keys().map(String::as_str)
    }

    /// Ensure `name` exists, generating a random 32-byte hex token and saving
    /// the file (mode 0600) if it does not. Returns the secret.
    pub fn ensure(&mut self, name: &str) -> Result<SecretString, ConfigError> {
        if let Some(s) = self.get(name) {
            return Ok(s);
        }
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| ConfigError::Random(e.to_string()))?;
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let secret = SecretString::from(token);
        self.map.insert(name.to_owned(), secret.clone());
        self.save()?;
        Ok(secret)
    }

    /// Path of the backing file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn save(&self) -> Result<(), ConfigError> {
        let file = SecretsFile {
            secrets: self
                .map
                .iter()
                .map(|(k, v)| (k.clone(), v.expose_secret().to_owned()))
                .collect(),
        };
        let text = toml::to_string(&file).map_err(|e| ConfigError::Invalid(e.to_string()))?;
        fsutil::atomic_write(&self.path, text.as_bytes(), Some(0o600))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let s = Secrets::load(&Paths::with_root(dir.path())).unwrap();
        assert!(s.get("daemon").is_none());
    }

    #[test]
    fn ensure_creates_0600_file_and_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(dir.path().join("cfg"));
        let mut s = Secrets::load(&paths).unwrap();
        let a = s.ensure("daemon").unwrap();
        assert_eq!(a.expose_secret().len(), 64);
        assert!(a.expose_secret().chars().all(|c| c.is_ascii_hexdigit()));
        let mode = std::fs::metadata(paths.secrets_file())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);

        let mut again = Secrets::load(&paths).unwrap();
        assert_eq!(
            again.get("daemon").unwrap().expose_secret(),
            a.expose_secret()
        );
        assert_eq!(
            again.ensure("daemon").unwrap().expose_secret(),
            a.expose_secret()
        );
        let b = again.ensure("other").unwrap();
        assert_ne!(a.expose_secret(), b.expose_secret());
    }

    #[test]
    fn refuses_group_or_other_access() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(dir.path());
        std::fs::write(paths.secrets_file(), "[secrets]\ndaemon = \"hunter2\"\n").unwrap();
        for mode in [0o644, 0o640, 0o604, 0o660] {
            std::fs::set_permissions(paths.secrets_file(), std::fs::Permissions::from_mode(mode))
                .unwrap();
            let err = Secrets::load(&paths).unwrap_err();
            assert!(
                matches!(err, ConfigError::InsecureSecrets { .. }),
                "{mode:o}"
            );
            assert!(err.to_string().contains("chmod 600"));
        }
        std::fs::set_permissions(paths.secrets_file(), std::fs::Permissions::from_mode(0o600))
            .unwrap();
        let s = Secrets::load(&paths).unwrap();
        assert_eq!(s.get("daemon").unwrap().expose_secret(), "hunter2");
    }

    #[test]
    fn debug_never_prints_values() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_root(dir.path());
        let mut s = Secrets::load(&paths).unwrap();
        let v = s.ensure("daemon").unwrap();
        let dbg = format!("{s:?}");
        assert!(dbg.contains("daemon"));
        assert!(!dbg.contains(v.expose_secret()));
    }
}
