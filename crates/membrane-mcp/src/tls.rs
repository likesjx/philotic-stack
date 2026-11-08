//! Optional direct transport security. Proxy identity never grants user authority.
use anyhow::{Context, Result, ensure};
use axum::{Extension, Router, extract::ConnectInfo};
use hyper_util::{rt::TokioIo, service::TowerToHyperService};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    io::BufReader,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{net::TcpListener, sync::oneshot, task::JoinSet};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{self, RootCertStore, server::WebPkiClientVerifier},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    certificate: PathBuf,
    private_key: PathBuf,
    client_ca: PathBuf,
    allowed_client_sha256: Vec<String>,
}

#[derive(Clone)]
pub struct Transport {
    acceptor: TlsAcceptor,
    allowed: Arc<Vec<[u8; 32]>>,
}

pub fn validate_bind(ip: Option<IpAddr>, tls: bool, hotel: bool) -> Result<()> {
    if let Some(ip) = ip {
        ensure!(
            !ip.is_unspecified() && !ip.is_multicast(),
            "explicit bind must identify one interface"
        );
        ensure!(
            ip.is_loopback() || (tls && hotel && matches!(ip, IpAddr::V4(v) if v.is_private())),
            "non-loopback bind requires hotel-managed TLS on a private IPv4 interface"
        );
    }
    Ok(())
}

impl Transport {
    pub fn load(path: &Path) -> Result<Self> {
        let c: Config = serde_json::from_slice(&std::fs::read(path)?)?;
        ensure!(
            (1..=8).contains(&c.allowed_client_sha256.len()),
            "one to eight proxy certificate pins required"
        );
        for p in [&c.certificate, &c.private_key, &c.client_ca] {
            ensure!(p.is_absolute(), "TLS paths must be absolute");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                std::fs::metadata(&c.private_key)?.permissions().mode() & 0o077 == 0,
                "TLS private key permissions must exclude group/other"
            );
        }
        let allowed = c
            .allowed_client_sha256
            .iter()
            .map(|p| {
                ensure!(
                    p.len() == 64
                        && p.bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                    "invalid proxy pin"
                );
                Ok(hex::decode(p)?.try_into().expect("validated pin length"))
            })
            .collect::<Result<Vec<[u8; 32]>>>()?;
        let certs = rustls_pemfile::certs(&mut BufReader::new(std::fs::File::open(c.certificate)?))
            .collect::<std::io::Result<Vec<_>>>()?;
        let key =
            rustls_pemfile::private_key(&mut BufReader::new(std::fs::File::open(c.private_key)?))?
                .context("missing private key")?;
        let mut roots = RootCertStore::empty();
        for cert in rustls_pemfile::certs(&mut BufReader::new(std::fs::File::open(c.client_ca)?)) {
            roots.add(cert?)?;
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier =
            WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider.clone())
                .build()?;
        let mut server = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, key)?;
        server.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Self {
            acceptor: TlsAcceptor::from(Arc::new(server)),
            allowed: Arc::new(allowed),
        })
    }

    // Shared by the TCP listener and the in-memory component test.
    async fn serve_stream<S>(self, stream: S, peer: std::net::SocketAddr, router: Router)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let Ok(Ok(tls)) =
            tokio::time::timeout(Duration::from_secs(5), self.acceptor.accept(stream)).await
        else {
            return;
        };
        let Some(leaf) = tls.get_ref().1.peer_certificates().and_then(|c| c.first()) else {
            return;
        };
        let pin: [u8; 32] = Sha256::digest(leaf.as_ref()).into();
        if !self.allowed.contains(&pin) {
            return;
        }
        let service = TowerToHyperService::new(
            router
                .layer(Extension(ConnectInfo(peer)))
                .layer(Extension(crate::server::ProxyTransport)),
        );
        let _ = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(tls), service)
            .await;
    }

    pub async fn serve(
        self,
        listener: TcpListener,
        router: Router,
        mut stop: oneshot::Receiver<()>,
    ) -> Result<()> {
        // Narrowing waits until every owned connection has terminated.
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                _ = &mut stop => break,
                Some(_) = connections.join_next(), if !connections.is_empty() => {},
                accepted = listener.accept(), if connections.len() < 128 => {
                    let (stream, peer) = accepted?;
                    let this = self.clone(); let router = router.clone();
                    connections.spawn(async move { this.serve_stream(stream, peer, router).await; });
                }
            }
        }
        connections.abort_all();
        while connections.join_next().await.is_some() {}
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn narrow_bind_cannot_bypass_transport_or_hotel_policy() {
        for ip in ["0.0.0.0", "::", "224.1.1.1", "8.8.8.8"] {
            assert!(validate_bind(Some(ip.parse().unwrap()), true, true).is_err());
        }
        let bridge = Some("172.18.0.1".parse().unwrap());
        assert!(validate_bind(bridge, false, true).is_err());
        assert!(validate_bind(bridge, true, false).is_err());
        assert!(validate_bind(bridge, true, true).is_ok());
        assert!(validate_bind(Some("127.0.0.1".parse().unwrap()), false, false).is_ok());
    }

    #[tokio::test]
    async fn shutdown_awaits_inflight_tls_connection_and_no_plaintext_fallback() {
        use rcgen::{BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair};
        use std::os::unix::fs::PermissionsExt;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = tempfile::tempdir().unwrap();
        let ca_key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec![]).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = params.self_signed(&ca_key).unwrap();
        let server_key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec!["localhost".into()]).unwrap();
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let server = params.signed_by(&server_key, &ca, &ca_key).unwrap();
        let client_key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec![]).unwrap();
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let client = params.signed_by(&client_key, &ca, &ca_key).unwrap();
        for (name, bytes) in [
            ("server.pem", server.pem()),
            ("key.pem", server_key.serialize_pem()),
            ("ca.pem", ca.pem()),
        ] {
            let p = dir.path().join(name);
            std::fs::write(&p, bytes).unwrap();
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let config = dir.path().join("tls.json");
        std::fs::write(&config,serde_json::to_vec(&serde_json::json!({"certificate":dir.path().join("server.pem"),"private_key":dir.path().join("key.pem"),"client_ca":dir.path().join("ca.pem"),"allowed_client_sha256":[hex::encode(Sha256::digest(client.der()))]})).unwrap()).unwrap();
        let transport = Transport::load(&config).unwrap();
        let mut roots = RootCertStore::empty();
        roots.add(ca.der().clone()).unwrap();
        let c = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_client_auth_cert(
            vec![client.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(client_key.serialize_der()).into(),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        let marker = entered.clone();
        let router = Router::new().route(
            "/wait",
            axum::routing::get(move || {
                let marker = marker.clone();
                async move {
                    marker.notify_one();
                    std::future::pending::<&'static str>().await
                }
            }),
        );
        let (tx, rx) = oneshot::channel();
        let serving = tokio::spawn(transport.serve(listener, router, rx));
        let mut plain = tokio::net::TcpStream::connect(addr).await.unwrap();
        plain
            .write_all(b"GET /wait HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut b = [0; 1024];
        let plain_result = tokio::time::timeout(Duration::from_secs(2), plain.read(&mut b))
            .await
            .unwrap();
        if let Ok(n) = plain_result {
            assert!(!b[..n].starts_with(b"HTTP/"));
        }
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut stream = tokio_rustls::TlsConnector::from(Arc::new(c))
            .connect("localhost".try_into().unwrap(), stream)
            .await
            .unwrap();
        stream
            .write_all(b"GET /wait HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), serving)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut b))
            .await
            .unwrap();
        assert!(result.is_err() || result.unwrap() == 0);
        std::fs::write(&config, b"{}").unwrap();
        assert!(Transport::load(&config).is_err());
    }
}

#[cfg(test)]
mod synthetic_ca_loader_tests {
    use super::*;
    use rcgen::{BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn synthetic_transport_loads_new_ca_and_refuses_bad_inputs_without_listener() {
        let dir = tempfile::tempdir().unwrap();
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(vec![]).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let server_key = KeyPair::generate().unwrap();
        let mut server_params = CertificateParams::new(vec!["localhost".into()]).unwrap();
        server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let server = server_params.signed_by(&server_key, &ca, &ca_key).unwrap();
        let wrong_key = KeyPair::generate().unwrap();
        let mut client_params = CertificateParams::new(vec![]).unwrap();
        client_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let client = client_params
            .signed_by(&KeyPair::generate().unwrap(), &ca, &ca_key)
            .unwrap();
        for (name, bytes) in [
            ("server.pem", server.pem()),
            ("key.pem", server_key.serialize_pem()),
            ("ca.pem", ca.pem()),
        ] {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let config = dir.path().join("tls.json");
        let mut data = serde_json::json!({"certificate":dir.path().join("server.pem"), "private_key":dir.path().join("key.pem"), "client_ca":dir.path().join("ca.pem"), "allowed_client_sha256":[hex::encode(Sha256::digest(client.der()))]});
        let write_config = |value: &serde_json::Value| {
            std::fs::write(&config, serde_json::to_vec(value).unwrap()).unwrap()
        };
        write_config(&data);
        let loaded = Transport::load(&config).unwrap();
        let expected: [u8; 32] = Sha256::digest(client.der()).into();
        assert_eq!(loaded.allowed.as_slice(), &[expected]);
        std::fs::write(dir.path().join("key.pem"), wrong_key.serialize_pem()).unwrap();
        assert!(Transport::load(&config).is_err());
        std::fs::write(dir.path().join("key.pem"), server_key.serialize_pem()).unwrap();
        data["allowed_client_sha256"] = serde_json::json!(["invalid"]);
        write_config(&data);
        assert!(Transport::load(&config).is_err());
        data["allowed_client_sha256"] =
            serde_json::json!([hex::encode(Sha256::digest(client.der()))]);
        write_config(&data);
        std::fs::write(dir.path().join("ca.pem"), b"not a certificate").unwrap();
        assert!(Transport::load(&config).is_err());
    }
}

#[cfg(test)]
mod synthetic_in_memory_handshake_tests {
    use super::*;
    use rcgen::{BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair};
    use std::os::unix::fs::PermissionsExt;
    use tokio_rustls::TlsConnector;

    #[tokio::test]
    async fn actual_transport_handshakes_with_new_ca_and_rejects_other_ca_without_listener() {
        let dir = tempfile::tempdir().unwrap();
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(vec![]).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let other_key = KeyPair::generate().unwrap();
        let mut other_params = CertificateParams::new(vec![]).unwrap();
        other_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let other_ca = other_params.self_signed(&other_key).unwrap();
        let server_key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec!["localhost".into()]).unwrap();
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let server = params.signed_by(&server_key, &ca, &ca_key).unwrap();
        let client_key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec![]).unwrap();
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let client = params.signed_by(&client_key, &ca, &ca_key).unwrap();
        let old_client_key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec![]).unwrap();
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let old_client = params
            .signed_by(&old_client_key, &other_ca, &other_key)
            .unwrap();
        for (name, bytes) in [
            ("server.pem", server.pem()),
            ("key.pem", server_key.serialize_pem()),
            ("ca.pem", ca.pem()),
        ] {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let config = dir.path().join("tls.json");
        std::fs::write(&config, serde_json::to_vec(&serde_json::json!({"certificate":dir.path().join("server.pem"), "private_key":dir.path().join("key.pem"), "client_ca":dir.path().join("ca.pem"), "allowed_client_sha256":[hex::encode(Sha256::digest(client.der()))]})).unwrap()).unwrap();
        let transport = Transport::load(&config).unwrap();
        let make_client = |root: &rcgen::Certificate, cert: &rcgen::Certificate, key: &KeyPair| {
            let mut roots = RootCertStore::empty();
            roots.add(root.der().clone()).unwrap();
            let c = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_root_certificates(roots)
            .with_client_auth_cert(
                vec![cert.der().clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
            )
            .unwrap();
            TlsConnector::from(Arc::new(c))
        };
        async fn handshake(transport: &Transport, connector: TlsConnector) -> (bool, bool) {
            let (server_io, client_io) = tokio::io::duplex(65536);
            let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
            let (server, client) = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::join!(
                    transport.acceptor.accept(server_io),
                    connector.connect(name, client_io)
                )
            })
            .await
            .expect("in-memory handshake timed out");
            (server.is_ok(), client.is_ok())
        }
        assert_eq!(
            handshake(&transport, make_client(&ca, &client, &client_key)).await,
            (true, true)
        );
        let (_, wrong_server_ca) =
            handshake(&transport, make_client(&other_ca, &client, &client_key)).await;
        assert!(!wrong_server_ca);
        let (old_client_accepted, _) =
            handshake(&transport, make_client(&ca, &old_client, &old_client_key)).await;
        assert!(!old_client_accepted);

        // Same trusted CA and EKU, but a different key/leaf than the allowed pin.
        let wrong_pin_key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec![]).unwrap();
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let wrong_pin_client = params.signed_by(&wrong_pin_key, &ca, &ca_key).unwrap();
        assert_ne!(
            Sha256::digest(client.der()),
            Sha256::digest(wrong_pin_client.der())
        );
        // Establish that this exact wrong-pin client completes mutual TLS on
        // both sides before testing the serving path's pin rejection.
        assert_eq!(
            handshake(
                &transport,
                make_client(&ca, &wrong_pin_client, &wrong_pin_key)
            )
            .await,
            (true, true)
        );
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handler_calls = calls.clone();
        let router = Router::new().route(
            "/",
            axum::routing::get(move || {
                let calls = handler_calls.clone();
                async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    "ok"
                }
            }),
        );
        async fn request(
            transport: &Transport,
            connector: TlsConnector,
            router: Router,
        ) -> Vec<u8> {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (server, client) = tokio::io::duplex(65536);
            // Both futures are owned here; timeout drops both, leaving no
            // detached server task.
            let (_, result) = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::join!(
                    transport.clone().serve_stream(
                        server,
                        "127.0.0.1:1234".parse().unwrap(),
                        router
                    ),
                    async {
                        let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
                        let mut tls = connector.connect(name, client).await?;
                        let _ = tls
                            .write_all(
                                b"GET / HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n",
                            )
                            .await;
                        let mut body = Vec::new();
                        let _ = tls.read_to_end(&mut body).await;
                        Ok::<_, std::io::Error>(body)
                    }
                )
            })
            .await
            .expect("request timed out");
            result.expect("client TLS handshake failed")
        }
        let response = request(
            &transport,
            make_client(&ca, &client, &client_key),
            router.clone(),
        )
        .await;
        assert!(response.starts_with(b"HTTP/1.1 200"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let response = request(
            &transport,
            make_client(&ca, &wrong_pin_client, &wrong_pin_key),
            router.clone(),
        )
        .await;
        assert!(response.is_empty());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let response = request(&transport, make_client(&ca, &client, &client_key), router).await;
        assert!(response.starts_with(b"HTTP/1.1 200"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}
