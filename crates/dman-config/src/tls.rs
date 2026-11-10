//! TLS settings: certificate/key files for listeners and CA/verification options for
//! remote profiles (rustls with the ring provider).

use std::path::{Path, PathBuf};

use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use serde::{Deserialize, Serialize};

use crate::expand_tilde;

/// Server certificate chain and private key (PEM files) for one listener.
///
/// ```toml
/// tls = { cert = "/etc/dman/fullchain.pem", key = "/etc/dman/privkey.pem" }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsFiles {
    /// PEM file with the certificate chain (leaf first). A leading `~` is expanded.
    pub cert: String,
    /// PEM file with the private key (PKCS#8, PKCS#1 or SEC1). A leading `~` is expanded.
    pub key: String,
}

impl TlsFiles {
    /// Certificate path with `~` expanded.
    pub fn cert_path(&self) -> PathBuf {
        expand_tilde(&self.cert)
    }

    /// Key path with `~` expanded.
    pub fn key_path(&self) -> PathBuf {
        expand_tilde(&self.key)
    }

    /// Reads both files and checks that they parse (`what` names the setting in errors).
    pub fn validate(&self, what: &str) -> Result<(), String> {
        let certs = read_certs(&self.cert_path()).map_err(|e| format!("{what}.cert: {e}"))?;
        if certs.is_empty() {
            return Err(format!(
                "{what}.cert: no certificate found in {}",
                self.cert
            ));
        }
        read_key(&self.key_path()).map_err(|e| format!("{what}.key: {e}"))?;
        Ok(())
    }

    /// Parsed certificate chain and key.
    pub fn load(&self) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), String> {
        Ok((read_certs(&self.cert_path())?, read_key(&self.key_path())?))
    }
}

/// All PEM certificates in `path`.
pub fn read_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, String> {
    CertificateDer::pem_file_iter(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("invalid PEM in {}: {e}", path.display()))
}

/// The first private key in the PEM file `path`.
pub fn read_key(path: &Path) -> Result<PrivateKeyDer<'static>, String> {
    PrivateKeyDer::from_pem_file(path)
        .map_err(|e| format!("cannot read a private key from {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_garbage_files_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let junk = dir.path().join("junk.pem");
        std::fs::write(&junk, "not pem at all").unwrap();
        let t = TlsFiles {
            cert: dir.path().join("none.pem").display().to_string(),
            key: junk.display().to_string(),
        };
        assert!(t.validate("tls").unwrap_err().starts_with("tls.cert"));
        let t = TlsFiles {
            cert: junk.display().to_string(),
            key: junk.display().to_string(),
        };
        assert!(t.validate("tls").unwrap_err().contains("no certificate"));
    }
}
