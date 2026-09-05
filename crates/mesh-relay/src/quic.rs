//! QUIC socket adapter: wraps the transport-agnostic relay core
//! ([`crate::transport`]) around real quinn connections.
//!
//! Each QUIC connection carries a single bidirectional stream. The client opens
//! it (the client speaks first — [`ClientMsg::Hello`]), the server accepts it,
//! and both halves are joined into one `AsyncRead + AsyncWrite` value that the
//! relay core drives unchanged.
//!
//! TLS: the relay presents a self-signed certificate and clients **pin** it by
//! exact DER. There is no CA and no hostname trust — a private, fixed-endpoint
//! system where the operator distributes the relay's cert fingerprint out of
//! band (alongside the relay address in mesh-config). This gives the wire
//! encryption the signed-but-not-encrypted mesh lacks without a PKI.

use crate::KeyResolver;
use crate::transport::{RelayClient, RelayServer};
use anyhow::{Context, Result, anyhow};
use ed25519_dalek::SigningKey;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Once;
use tokio::io::join;
use tracing::{debug, warn};

static INSTALL_CRYPTO: Once = Once::new();

/// Install the ring crypto provider as the process default exactly once. rustls
/// 0.23 requires a default provider before its simple builders can be used.
fn ensure_crypto_provider() {
    INSTALL_CRYPTO.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// A self-signed relay identity: the cert clients pin, plus the private key the
/// server presents.
pub struct RelayIdentity {
    pub cert_der: CertificateDer<'static>,
    key_der: PrivatePkcs8KeyDer<'static>,
}

impl RelayIdentity {
    /// Generate a fresh self-signed identity. In production the relay persists
    /// one identity and the operator distributes [`RelayIdentity::cert_der`] to
    /// hotels as the pin.
    pub fn generate() -> Result<Self> {
        let cert = rcgen::generate_simple_self_signed(vec!["philotic-mesh-relay".to_string()])
            .context("generate self-signed relay cert")?;
        let cert_der = cert.cert.der().clone();
        let key_der = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
        Ok(Self { cert_der, key_der })
    }
}

/// Bind a QUIC relay endpoint on `addr`. Returns the endpoint plus the relay
/// core; call [`serve_relay`] to run the accept loop.
pub fn bind_relay(
    addr: SocketAddr,
    identity: &RelayIdentity,
) -> Result<(quinn::Endpoint, RelayServer)> {
    ensure_crypto_provider();
    let key = PrivateKeyDer::Pkcs8(identity.key_der.clone_key());
    let mut server_crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![identity.cert_der.clone()], key)
        .context("relay server rustls config")?;
    server_crypto.alpn_protocols = vec![b"philotic-mesh-relay".to_vec()];

    let server_config =
        quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(server_crypto)?));
    let endpoint = quinn::Endpoint::server(server_config, addr).context("bind relay endpoint")?;
    Ok((endpoint, RelayServer::new()))
}

/// Run the relay accept loop: for each incoming connection, accept its bi
/// stream and hand it to the relay core, authenticated against `resolver`.
/// Runs until the endpoint is closed.
pub async fn serve_relay(
    endpoint: quinn::Endpoint,
    server: RelayServer,
    resolver: Arc<dyn KeyResolver>,
) {
    while let Some(incoming) = endpoint.accept().await {
        let server = server.clone();
        let resolver = resolver.clone();
        tokio::spawn(async move {
            let conn = match incoming.await {
                Ok(c) => c,
                Err(e) => {
                    debug!("relay: incoming connection failed: {e}");
                    return;
                }
            };
            let peer = conn.remote_address();
            let (send, recv) = match conn.accept_bi().await {
                Ok(s) => s,
                Err(e) => {
                    debug!("relay: {peer} opened no stream: {e}");
                    return;
                }
            };
            let stream = join(recv, send);
            if let Err(e) = server.handle_connection(stream, resolver.as_ref()).await {
                debug!("relay: connection from {peer} ended: {e}");
            }
        });
    }
    warn!("relay: accept loop ended (endpoint closed)");
}

/// Rustls verifier that accepts exactly one pinned self-signed server cert.
#[derive(Debug)]
struct PinnedServerCert {
    pinned: CertificateDer<'static>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for PinnedServerCert {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if end_entity.as_ref() == self.pinned.as_ref() {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "relay: server certificate does not match pinned certificate".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Dial the relay at `relay_addr`, pinning its `pinned_cert`, and authenticate
/// as `node_id` with `signing_key`. Returns a ready [`RelayClient`] over the
/// QUIC bidirectional stream.
pub async fn connect_relay(
    relay_addr: SocketAddr,
    pinned_cert: CertificateDer<'static>,
    node_id: String,
    signing_key: &SigningKey,
) -> Result<RelayClient<tokio::io::Join<quinn::RecvStream, quinn::SendStream>>> {
    ensure_crypto_provider();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut client_crypto = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .context("relay client protocol versions")?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedServerCert {
            pinned: pinned_cert,
            provider,
        }))
        .with_no_client_auth();
    client_crypto.alpn_protocols = vec![b"philotic-mesh-relay".to_vec()];

    // Bind an ephemeral client socket. v4 unspecified is fine for dialing a v4
    // relay; a v6 relay would use "[::]:0".
    let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse().unwrap())
        .context("bind relay client endpoint")?;
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(
        QuicClientConfig::try_from(client_crypto)?,
    )));

    let conn = endpoint
        .connect(relay_addr, "philotic-mesh-relay")
        .context("start relay connection")?
        .await
        .context("relay connection handshake")?;
    let (send, recv) = conn.open_bi().await.context("open relay stream")?;
    let stream = join(recv, send);
    RelayClient::connect(stream, node_id, signing_key)
        .await
        .map_err(|e| anyhow!("relay client handshake: {e}"))
}
