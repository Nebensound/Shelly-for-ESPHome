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

/// Byte accounting of one response; only the newest request reports progress.
struct ResponseProgress {
    stats: Arc<TransferStats>,
    request_id: u64,
    sent: u64,
    total: u64,
}

impl ResponseProgress {
    fn record(&mut self, chunk_bytes: usize) {
        self.sent += chunk_bytes as u64;
        if self.stats.requests.load(Ordering::SeqCst) == self.request_id {
            self.stats.sent_bytes.store(self.sent, Ordering::SeqCst);
        }
        if self.sent == self.total {
            self.stats.completed.fetch_add(1, Ordering::SeqCst);
        }
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
    let mut progress = ResponseProgress {
        stats: Arc::clone(&file.stats),
        request_id,
        sent: 0,
        total: total as u64,
    };
    let stream = futures_util::stream::iter(chunks.map(move |chunk| {
        progress.record(chunk.len());
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
    /// Fails if the address cannot be bound.
    ///
    /// # Panics
    /// If the operating system provides no random numbers.
    pub async fn start(
        bind: SocketAddr,
        package: Bytes,
        image: Bytes,
        allowed_peer: Option<IpAddr>,
    ) -> std::io::Result<Self> {
        let mut random = [0_u8; TOKEN_BYTES];
        // Without an OS random source there is no safe token; nothing works then.
        getrandom::fill(&mut random).expect("the operating system's random source is unavailable");
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
        // A bound listener always knows its address.
        let address = listener.local_addr().unwrap_or(bind);
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
        // `send` only fails if the server task is gone already.
        let _ = self.shutdown.take().map(|shutdown| shutdown.send(()));
    }
}

#[cfg(test)]
#[path = "unit_tests/server.rs"]
mod tests;
