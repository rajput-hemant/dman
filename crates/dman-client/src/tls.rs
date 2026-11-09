//! TLS options for `wss://` connections (rustls with the ring provider, decision D18).
//!
//! Default trust is the bundled Mozilla root set (`webpki-roots`, pure data, no system
//! lookups). A custom CA replaces it entirely (pinning); `insecure_skip_verify` disables
//! verification altogether.

use std::path::Path;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, ring};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use rustls_pki_types::pem::PemObject;
use tokio_tungstenite::Connector;

use crate::error::ClientError;

/// How to verify the daemon's certificate on `wss://` connections. Ignored for `ws://`.
#[derive(Debug, Clone, Default)]
pub struct TlsOptions {
    ca: Option<Vec<CertificateDer<'static>>>,
    insecure: bool,
}

impl TlsOptions {
    /// Default verification against the bundled public roots.
    pub fn new() -> Self {
        Self::default()
    }

    /// Trust only the certificates in this PEM file (a private CA or the daemon's
    /// self-signed certificate).
    ///
    /// # Errors
    /// [`ClientError::Connect`] when the file cannot be read or holds no certificate.
    pub fn with_ca_file(mut self, path: &Path) -> Result<Self, ClientError> {
        let certs = CertificateDer::pem_file_iter(path)
            .map_err(|e| ClientError::Connect(format!("cannot read CA {}: {e}", path.display())))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| {
                ClientError::Connect(format!("invalid PEM in CA {}: {e}", path.display()))
            })?;
        if certs.is_empty() {
            return Err(ClientError::Connect(format!(
                "no certificate found in CA {}",
                path.display()
            )));
        }
        self.ca = Some(certs);
        Ok(self)
    }

    /// Accept any certificate (testing only; every connection logs a warning).
    #[must_use]
    pub fn insecure_skip_verify(mut self, yes: bool) -> Self {
        self.insecure = yes;
        self
    }

    pub(crate) fn connector(&self) -> Result<Connector, ClientError> {
        let provider = Arc::new(ring::default_provider());
        let builder = ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|e| ClientError::Connect(format!("TLS configuration: {e}")))?;
        let config = if self.insecure {
            tracing::warn!(
                "TLS certificate verification is DISABLED (insecure_skip_verify): \
                 the connection can be intercepted"
            );
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
                .with_no_client_auth()
        } else {
            let mut roots = RootCertStore::empty();
            match &self.ca {
                Some(ca) => {
                    for c in ca {
                        roots
                            .add(c.clone())
                            .map_err(|e| ClientError::Connect(format!("invalid CA: {e}")))?;
                    }
                }
                None => roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
            }
            builder.with_root_certificates(roots).with_no_client_auth()
        };
        Ok(Connector::Rustls(Arc::new(config)))
    }
}

/// Verifier that accepts everything (signatures are still checked structurally).
#[derive(Debug)]
struct NoVerify(Arc<CryptoProvider>);

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Finds a rustls error anywhere in an error chain (also inside `io::Error` payloads).
fn find_rustls<'a>(e: &'a (dyn std::error::Error + 'static)) -> Option<&'a rustls::Error> {
    let mut cur = Some(e);
    while let Some(x) = cur {
        if let Some(r) = x.downcast_ref::<rustls::Error>() {
            return Some(r);
        }
        if let Some(inner) = x
            .downcast_ref::<std::io::Error>()
            .and_then(std::io::Error::get_ref)
            && let Some(r) = find_rustls(inner)
        {
            return Some(r);
        }
        cur = x.source();
    }
    None
}

/// Turns a WebSocket connect error into a [`ClientError::Connect`] with a readable message.
pub(crate) fn connect_error(e: &tokio_tungstenite::tungstenite::Error) -> ClientError {
    match find_rustls(e) {
        Some(rustls::Error::InvalidCertificate(c)) => ClientError::Connect(format!(
            "certificate not trusted: {c:?} (is the CA configured? see `ca` in the profile)"
        )),
        Some(other) => ClientError::Connect(format!("TLS error: {other}")),
        None => ClientError::Connect(e.to_string()),
    }
}
