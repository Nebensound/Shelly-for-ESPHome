//! A simulated Shelly cloud: the update service and the firmware CDN behind
//! one TLS server, with version 1 certificates of a test CA like Shelly's.
//! `FetchOptions::resolve` sends both host names to it, so the production
//! URLs work unchanged.

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use rustls::{
    ServerConfig,
    crypto::ring,
    pki_types::{CertificateDer, PrivateKeyDer},
    server::{ClientHello, ResolvesServerCert},
    sign::CertifiedKey,
};
use shelly2esphome::{
    fetch::FetchOptions,
    shelly_tls::synthetic::{CertSpec, TestCa, private_key},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

pub const API_HOST: &str = "updates.shelly.cloud";
pub const CDN_HOST: &str = "fwcdn.shelly.cloud";
const KEY: u8 = 0x51;

/// Presents a certificate for whatever host the client asks for.
#[derive(Debug)]
struct ForAnyHost(TestCa);

impl ResolvesServerCert for ForAnyHost {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let cert = self.0.issue(&CertSpec::server(hello.server_name()?));
        let key = ring::default_provider()
            .key_provider
            .load_private_key(PrivateKeyDer::Pkcs8(private_key(KEY).into()))
            .ok()?;
        Some(Arc::new(CertifiedKey::new(vec![CertificateDer::from(cert)], key)))
    }
}

pub struct Cloud {
    pub addr: SocketAddr,
    pub ca: TestCa,
    routes: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Cloud {
    pub async fn start() -> Self {
        let ca = TestCa::new(1, "Allterco");
        let config = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(ForAnyHost(ca.clone())));
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let routes: Arc<Mutex<HashMap<String, Vec<u8>>>> = Arc::default();
        let requests: Arc<Mutex<Vec<String>>> = Arc::default();
        let (shared_routes, shared_requests) = (routes.clone(), requests.clone());
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let (acceptor, routes, requests) =
                    (acceptor.clone(), shared_routes.clone(), shared_requests.clone());
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(tcp).await else { return };
                    let mut request = Vec::new();
                    let mut buffer = [0; 1024];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        match tls.read(&mut buffer).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => request.extend_from_slice(&buffer[..read]),
                        }
                    }
                    let line = String::from_utf8_lossy(&request).lines().next().unwrap().to_owned();
                    let path = line.split(' ').nth(1).unwrap().to_owned();
                    requests.lock().unwrap().push(path.clone());
                    let response = routes.lock().unwrap().get(&path).cloned();
                    let _ = tls.write_all(&response.unwrap_or_else(|| status(404))).await;
                    let _ = tls.shutdown().await;
                });
            }
        });
        Self { addr, ca, routes, requests }
    }

    /// Answers GET `path` with the raw HTTP `response`.
    pub fn route(&self, path: &str, response: Vec<u8>) {
        self.routes.lock().unwrap().insert(path.to_owned(), response);
    }

    /// The paths requested so far.
    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    pub fn options(&self, cache: Option<PathBuf>) -> FetchOptions {
        FetchOptions {
            root: self.ca.root(),
            resolve: vec![(API_HOST.into(), self.addr), (CDN_HOST.into(), self.addr)],
            cache,
            timeout: Duration::from_secs(5),
        }
    }

    /// Publishes `package` as the stable `version` of `app` and returns its URL.
    pub fn publish(&self, app: &str, version: &str, package: &[u8]) -> String {
        let digest = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(package));
        let path = format!("/gen2/{app}/{digest}");
        self.release(app, version, &format!("https://{CDN_HOST}{path}"));
        self.route(&path, ok(package));
        format!("https://{CDN_HOST}{path}")
    }

    /// The update service names `url` as the stable `version` of `app`.
    pub fn release(&self, app: &str, version: &str, url: &str) {
        let reply = serde_json::json!({
            "stable": { "version": version, "build_id": "20261026-000000/x", "url": url },
            "time": 1_794_396_354,
        });
        self.route(&format!("/update/{app}"), ok(reply.to_string().as_bytes()));
    }
}

/// `200 OK` with `body`.
pub fn ok(body: &[u8]) -> Vec<u8> {
    truncated(body, body.len())
}

/// `200 OK` announcing `announced` bytes but sending `body`.
pub fn truncated(body: &[u8], announced: usize) -> Vec<u8> {
    let head =
        format!("HTTP/1.1 200 OK\r\nContent-Length: {announced}\r\nConnection: close\r\n\r\n");
    [head.as_bytes(), body].concat()
}

/// An empty answer with `code`.
pub fn status(code: u16) -> Vec<u8> {
    let reason = reqwest::StatusCode::from_u16(code).unwrap().canonical_reason().unwrap();
    format!("HTTP/1.1 {code} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .into_bytes()
}
