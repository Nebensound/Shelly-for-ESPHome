//! Minimal HTTP server that hands the migration package and the image to the
//! device and records how much of each file was sent.
//!
//! The image contains the user's Wi-Fi credentials, so the files are only
//! served under an unpredictable per-run path and, if configured, only to the
//! device's IP address.

use std::{
    net::{IpAddr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
};

use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use bytes::Bytes;
use tokio::{net::TcpListener, sync::oneshot};

/// File name under which the migration package is served (behind the token).
pub const PACKAGE_PATH: &str = "/mg2esphome.zip";
/// File name under which the firmware image is served (behind the token).
pub const IMAGE_PATH: &str = "/firmware.bin";
const CHUNK_BYTES: usize = 4096;
const TOKEN_BYTES: usize = 16;

/// Transfer statistics of one served file.
#[derive(Debug, Default)]
pub struct TransferStats {
    requests: AtomicU64,
    sent_bytes: AtomicU64,
    completed: AtomicU32,
}

impl TransferStats {
    /// Number of accepted requests for the file.
    #[must_use]
    pub fn requests(&self) -> u64 {
        self.requests.load(Ordering::SeqCst)
    }

    /// Bytes handed to the network for the most recent request.
    #[must_use]
    pub fn sent_bytes(&self) -> u64 {
        self.sent_bytes.load(Ordering::SeqCst)
    }

    /// Number of responses whose body was handed over completely.
    #[must_use]
    pub fn completed(&self) -> u32 {
        self.completed.load(Ordering::SeqCst)
    }
}

#[derive(Clone)]
struct ServedFile {
    bytes: Bytes,
    stats: Arc<TransferStats>,
    allowed_peer: Option<IpAddr>,
    rejected: Arc<AtomicU32>,
}

async fn serve_file(
    State(file): State<ServedFile>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Response {
    if file.allowed_peer.is_some_and(|allowed| allowed != peer.ip()) {
        file.rejected.fetch_add(1, Ordering::SeqCst);
        return StatusCode::FORBIDDEN.into_response();
    }
    // Each response tracks its own progress; only the newest one reports it.
    let request_id = file.stats.requests.fetch_add(1, Ordering::SeqCst) + 1;
    file.stats.sent_bytes.store(0, Ordering::SeqCst);
    let total = file.bytes.len();
    // Chunks are zero-copy slices of the shared buffer, created lazily.
    let bytes = file.bytes.clone();
    let chunks = (0..total)
        .step_by(CHUNK_BYTES)
        .map(move |start| bytes.slice(start..total.min(start + CHUNK_BYTES)));
    let stats = Arc::clone(&file.stats);
    let mut sent_by_this_response = 0_u64;
    let stream = futures_util::stream::iter(chunks.map(move |chunk| {
        sent_by_this_response += chunk.len() as u64;
        if stats.requests.load(Ordering::SeqCst) == request_id {
            stats.sent_bytes.store(sent_by_this_response, Ordering::SeqCst);
        }
        if sent_by_this_response == total as u64 {
            stats.completed.fetch_add(1, Ordering::SeqCst);
        }
        Ok::<Bytes, std::convert::Infallible>(chunk)
    }));
    (
        [
            (axum::http::header::CONTENT_TYPE, "application/octet-stream".to_owned()),
            (axum::http::header::CONTENT_LENGTH, total.to_string()),
            // The image carries the Wi-Fi credentials: nothing may keep a copy.
            (axum::http::header::CACHE_CONTROL, "no-store".to_owned()),
            (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

/// A running file server; stops when dropped.
#[derive(Debug)]
pub struct FileServer {
    address: SocketAddr,
    token: String,
    package: Arc<TransferStats>,
    image: Arc<TransferStats>,
    rejected: Arc<AtomicU32>,
    shutdown: Option<oneshot::Sender<()>>,
}

impl FileServer {
    /// Binds `bind` (port 0 picks a free port) and serves both files under a
    /// random path, only to `allowed_peer` if given.
    ///
    /// # Errors
    /// Fails if the address cannot be bound or no random token can be drawn.
    pub async fn start(
        bind: SocketAddr,
        package: Bytes,
        image: Bytes,
        allowed_peer: Option<IpAddr>,
    ) -> std::io::Result<Self> {
        let mut random = [0_u8; TOKEN_BYTES];
        getrandom::fill(&mut random).map_err(|error| std::io::Error::other(error.to_string()))?;
        let token = hex::encode(random);
        let rejected = Arc::new(AtomicU32::new(0));
        let package_file = ServedFile {
            bytes: package,
            stats: Arc::default(),
            allowed_peer,
            rejected: Arc::clone(&rejected),
        };
        let image_file = ServedFile {
            bytes: image,
            stats: Arc::default(),
            allowed_peer,
            rejected: Arc::clone(&rejected),
        };
        let package_stats = Arc::clone(&package_file.stats);
        let image_stats = Arc::clone(&image_file.stats);
        let app = Router::new()
            .route(&format!("/{token}{PACKAGE_PATH}"), get(serve_file).with_state(package_file))
            .route(&format!("/{token}{IMAGE_PATH}"), get(serve_file).with_state(image_file));
        let listener = TcpListener::bind(bind).await?;
        let address = listener.local_addr()?;
        let (shutdown, stopped) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await;
        });
        Ok(Self {
            address,
            token,
            package: package_stats,
            image: image_stats,
            rejected,
            shutdown: Some(shutdown),
        })
    }

    /// Address the server listens on.
    #[must_use]
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// URL of a served file as reachable from the device.
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        format!("http://{}/{}{path}", self.address, self.token)
    }

    /// Statistics of the migration package.
    #[must_use]
    pub fn package_stats(&self) -> &TransferStats {
        &self.package
    }

    /// Statistics of the firmware image.
    #[must_use]
    pub fn image_stats(&self) -> &TransferStats {
        &self.image
    }

    /// Requests refused because they did not come from the device.
    #[must_use]
    pub fn rejected_requests(&self) -> u32 {
        self.rejected.load(Ordering::SeqCst)
    }
}

impl Drop for FileServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn server(allowed_peer: Option<IpAddr>) -> FileServer {
        let image = Bytes::from(vec![7_u8; 10_000]);
        FileServer::start(
            "127.0.0.1:0".parse().expect("addr"),
            Bytes::from_static(b"zip"),
            image,
            allowed_peer,
        )
        .await
        .expect("server")
    }

    #[tokio::test]
    async fn serves_both_files_and_counts_bytes() {
        let server = server(Some("127.0.0.1".parse().expect("ip"))).await;
        let response = reqwest::get(server.url(IMAGE_PATH)).await.expect("get");
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        let body = response.bytes().await.expect("body");
        assert_eq!(body.len(), 10_000);
        assert_eq!(server.image_stats().requests(), 1);
        assert_eq!(server.image_stats().sent_bytes(), 10_000);
        assert_eq!(server.image_stats().completed(), 1);

        let zip =
            reqwest::get(server.url(PACKAGE_PATH)).await.expect("get").bytes().await.expect("body");
        assert_eq!(&zip[..], b"zip");
        assert_eq!(server.package_stats().completed(), 1);
        assert_eq!(server.address().ip().to_string(), "127.0.0.1");
        assert_eq!(server.rejected_requests(), 0);
    }

    #[tokio::test]
    async fn files_need_the_token() {
        let server = server(None).await;
        let address = server.address();
        for path in [IMAGE_PATH, PACKAGE_PATH] {
            let response = reqwest::get(format!("http://{address}{path}")).await.expect("get");
            assert_eq!(response.status(), 404);
        }
        let wrong = format!("http://{address}/{}{IMAGE_PATH}", "0".repeat(32));
        assert_eq!(reqwest::get(wrong).await.expect("get").status(), 404);
        assert_eq!(server.image_stats().requests(), 0);
        // Two servers never share a token.
        assert_ne!(server.url(IMAGE_PATH), self::server(None).await.url(IMAGE_PATH));
    }

    #[tokio::test]
    async fn other_hosts_are_refused() {
        let server = server(Some("10.10.5.79".parse().expect("ip"))).await;
        let response = reqwest::get(server.url(IMAGE_PATH)).await.expect("get");
        assert_eq!(response.status(), 403);
        assert_eq!(server.rejected_requests(), 1);
        assert_eq!(server.image_stats().requests(), 0);
    }

    #[tokio::test]
    async fn overlapping_downloads_are_counted_per_response() {
        let server = server(None).await;
        let (first, second) = tokio::join!(
            reqwest::get(server.url(IMAGE_PATH)),
            reqwest::get(server.url(IMAGE_PATH))
        );
        let (first, second) = (first.expect("first"), second.expect("second"));
        assert_eq!(first.bytes().await.expect("body").len(), 10_000);
        assert_eq!(second.bytes().await.expect("body").len(), 10_000);
        assert_eq!(server.image_stats().completed(), 2);
        assert_eq!(server.image_stats().requests(), 2);
        assert_eq!(server.image_stats().sent_bytes(), 10_000);
    }
}
