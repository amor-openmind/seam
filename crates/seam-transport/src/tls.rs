//! TLS configuration for the QUIC transport.
//!
//! # What these verifiers do, and what they deliberately do not
//!
//! seam authenticates peers by **pinned certificate**, decided after the handshake by
//! comparing against the [`crate::TrustStore`]. So the verifiers here do not check a
//! certificate chain, a name, or an expiry date — there is no CA, the SAN is a fixed
//! placeholder, and an expiry would only create a renewal failure mode for a check that
//! proves nothing.
//!
//! They **do** verify the handshake signature. That is the part that matters, and the
//! part that is easy to get wrong: accepting any certificate is not the same as accepting
//! an unproven one. Without signature verification anyone could replay a copy of a
//! trusted peer's public certificate — which is not secret — and be pinned as that peer.
//! With it, the peer must prove possession of the private key.
//!
//! Both sides present a certificate: the link is mutually authenticated, so "client" and
//! "server" are QUIC roles rather than trust roles. Peers are symmetric (goal O3).

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};

use crate::{Error, Fingerprint, Identity};

/// The name presented in SNI.
///
/// Never validated — see the module docs. It is a constant so that nothing about the
/// machine's hostname can leak into the handshake or affect whether it succeeds.
pub(crate) const SERVER_NAME: &str = "seam.invalid";

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Verifies the peer proved possession of its private key, and nothing else.
///
/// Naming a type `Dangerous…` and then using it everywhere defeats the point of the
/// marker, so the reasoning lives in the module docs rather than in a scary name: this
/// is the *correct* verifier for a pinned-key protocol, not a shortcut around one.
#[derive(Debug)]
struct ProofOfKeyOnly {
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ProofOfKeyOnly {
    fn new() -> Arc<Self> {
        Arc::new(Self { provider: provider() })
    }

    fn verify_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }
}

impl ServerCertVerifier for ProofOfKeyOnly {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        // Identity is decided after the handshake against the trust store, from the
        // certificate quinn records on the connection. Nothing to check here.
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        // QUIC is TLS 1.3 only, and the config below restricts to it, so this is
        // unreachable. Refuse rather than accept: an unreachable branch that returns
        // "valid" is one config change away from being a vulnerability.
        Err(rustls::Error::PeerIncompatible(rustls::PeerIncompatible::Tls12NotOffered))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verify_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

impl ClientCertVerifier for ProofOfKeyOnly {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        // No CAs, so no hints to offer. Peers always have exactly one certificate.
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(rustls::PeerIncompatible::Tls12NotOffered))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verify_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn credentials(identity: &Identity) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    let chain = vec![CertificateDer::from(identity.certificate_der().to_vec())];
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.private_key_der().to_vec()));
    (chain, key)
}

/// Build the rustls server configuration: TLS 1.3 only, client certificate **required**.
pub(crate) fn server_config(identity: &Identity) -> Result<rustls::ServerConfig, Error> {
    let (chain, key) = credentials(identity);
    rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| tls_err(&e))?
        // Required, not optional: an unauthenticated peer is never acceptable, because
        // this software forwards keystrokes.
        .with_client_cert_verifier(ProofOfKeyOnly::new())
        .with_single_cert(chain, key)
        .map_err(|e| tls_err(&e))
}

/// Build the rustls client configuration: TLS 1.3 only, always presents a certificate.
pub(crate) fn client_config(identity: &Identity) -> Result<rustls::ClientConfig, Error> {
    let (chain, key) = credentials(identity);
    rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| tls_err(&e))?
        .dangerous()
        .with_custom_certificate_verifier(ProofOfKeyOnly::new())
        .with_client_auth_cert(chain, key)
        .map_err(|e| tls_err(&e))
}

fn tls_err(e: &rustls::Error) -> Error {
    Error::Tls(e.to_string())
}

/// Extract a peer's fingerprint from the certificate quinn recorded on the connection.
///
/// Returns `None` only if the peer presented no certificate, which the configuration
/// above makes impossible — but it is reported rather than asserted, because an
/// unauthenticated peer must fail closed if a future config change ever allows one.
#[must_use]
pub(crate) fn peer_fingerprint(connection: &quinn::Connection) -> Option<Fingerprint> {
    let identity = connection.peer_identity()?;
    let chain = identity.downcast::<Vec<CertificateDer<'static>>>().ok()?;
    chain.first().map(|cert| Fingerprint::of_certificate(cert))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configs_build_from_a_generated_identity() {
        let id = Identity::generate().unwrap();
        assert!(server_config(&id).is_ok());
        assert!(client_config(&id).is_ok());
    }

    // The two properties that matter here — that a TLS 1.2 signature is refused, and
    // that a forged TLS 1.3 signature is rejected — cannot be unit-tested: rustls keeps
    // `DigitallySignedStruct::new` private, so a signature cannot be fabricated. They are
    // covered instead by `tests/handshake.rs`, which exercises them over a real
    // connection. That is the stronger test anyway.

    #[test]
    fn some_signature_schemes_are_supported() {
        let v = ProofOfKeyOnly::new();
        assert!(!ServerCertVerifier::supported_verify_schemes(&*v).is_empty());
        assert!(!ClientCertVerifier::supported_verify_schemes(&*v).is_empty());
    }
}
