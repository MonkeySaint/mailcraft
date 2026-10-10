//! Self-signed certificate and rustls server configuration (ring provider only).

use std::sync::Arc;

use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use crate::{Error, Result};

pub struct Identity {
    pub cert_pem: String,
    pub cert_der: Vec<u8>,
    key_der: Vec<u8>,
}

impl Identity {
    /// A fresh self-signed certificate for `localhost`, `127.0.0.1` and `hostname`.
    pub fn generate(hostname: &str) -> Result<Identity> {
        let mut names = vec!["localhost".to_owned(), "127.0.0.1".to_owned()];
        let host = hostname.trim();
        if !host.is_empty() && !names.iter().any(|n| n.eq_ignore_ascii_case(host)) {
            names.push(host.to_owned());
        }
        let ck = rcgen::generate_simple_self_signed(names).map_err(|e| Error::Cert(e.to_string()))?;
        Ok(Identity {
            cert_pem: ck.cert.pem(),
            cert_der: ck.cert.der().to_vec(),
            key_der: ck.signing_key.serialize_der(),
        })
    }

    pub fn server_config(&self) -> Result<Arc<ServerConfig>> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let cert = CertificateDer::from(self.cert_der.clone());
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key_der.clone()));
        let cfg = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| Error::Tls(e.to_string()))?
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .map_err(|e| Error::Tls(e.to_string()))?;
        Ok(Arc::new(cfg))
    }
}
