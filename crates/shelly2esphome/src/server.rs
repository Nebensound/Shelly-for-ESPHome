//! Minimal HTTP server that hands the migration package and the image to the
//! device and records how much of each file was sent.

use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
};

use axum::{Router, body::Body, extract::State, response::IntoResponse, routing::get};
use bytes::Bytes;
use tokio::{net::TcpListener, sync::oneshot};

/// Path under which the migration package is served.
pub const PACKAGE_PATH: &str = "/mg2esphome.zip";
/// Path under which the firmware image is served.
pub const IMAGE_PATH: &str = "/firmware.bin";
const CHUNK_BYTES: usize = 4096;

/// Transfer statistics of one served file.
#[derive(Debug, Default)]
pub struct TransferStats {
    requests: AtomicU32,
    sent_bytes: AtomicU64,
    completed: AtomicU32,
}

impl TransferStats {
    /// Number of requests for the file.
    #[must_use]
    pub fn requests(&self) -> u32 {
        self.requests.load(Ordering::SeqCst)
    }

    /// Bytes handed to the network for the most recent request.
    #[must_use]
    pub fn sent_bytes(&self) -> u64 {
        self.sent_bytes.load(Ordering::SeqCst)
    }

    /// Number of requests whose body was handed over completely.
    #[must_use]
    pub fn completed(&self) -> u32 {
        self.completed.load(Ordering::SeqCst)
    }
}

#[derive(Clone)]
struct ServedFile {
    bytes: Bytes,
    stats: Arc<TransferStats>,
}

async fn serve_file(State(file): State<ServedFile>) -> impl IntoResponse {
    file.stats.requests.fetch_add(1, Ordering::SeqCst);
    file.stats.sent_bytes.store(0, Ordering::SeqCst);
    let total = file.bytes.len();
    let chunks: Vec<Bytes> = (0..total)
        .step_by(CHUNK_BYTES)
        .map(|start| file.bytes.slice(start..total.min(start + CHUNK_BYTES)))
        .collect();
    let stats = Arc::clone(&file.stats);
    let stream = futures_util::stream::iter(chunks.into_iter().map(move |chunk| {
        let sent =
            stats.sent_bytes.fetch_add(chunk.len() as u64, Ordering::SeqCst) + chunk.len() as u64;
        if sent == total as u64 {
            stats.completed.fetch_add(1, Ordering::SeqCst);
        }
        Ok::<Bytes, std::convert::Infallible>(chunk)
    }));
    (
        [
            (axum::http::header::CONTENT_TYPE, "application/octet-stream".to_owned()),
            (axum::http::header::CONTENT_LENGTH, total.to_string()),
        ],
        Body::from_stream(stream),
    )
}

/// A running file server; stops when dropped.
#[derive(Debug)]
pub struct FileServer {
    address: SocketAddr,
    package: Arc<TransferStats>,
    image: Arc<TransferStats>,
    shutdown: Option<oneshot::Sender<()>>,
}

impl FileServer {
    /// Binds `bind` (port 0 picks a free port) and serves both files.
    ///
    /// # Errors
    /// Fails if the address cannot be bound.
    pub async fn start(bind: SocketAddr, package: Bytes, image: Bytes) -> std::io::Result<Self> {
        let package_file = ServedFile { bytes: package, stats: Arc::default() };
        let image_file = ServedFile { bytes: image, stats: Arc::default() };
        let package_stats = Arc::clone(&package_file.stats);
        let image_stats = Arc::clone(&image_file.stats);
        let app = Router::new()
            .route(PACKAGE_PATH, get(serve_file).with_state(package_file))
            .route(IMAGE_PATH, get(serve_file).with_state(image_file));
        let listener = TcpListener::bind(bind).await?;
        let address = listener.local_addr()?;
        let (shutdown, stopped) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await;
        });
        Ok(Self { address, package: package_stats, image: image_stats, shutdown: Some(shutdown) })
    }

    /// Address the server listens on.
    #[must_use]
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// URL of a served path as reachable from the device.
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
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

    #[tokio::test]
    async fn serves_both_files_and_counts_bytes() {
        let package = Bytes::from_static(b"zip");
        let image = Bytes::from(vec![7_u8; 10_000]);
        let server =
            FileServer::start("127.0.0.1:0".parse().expect("addr"), package, image.clone())
                .await
                .expect("server");

        let body =
            reqwest::get(server.url(IMAGE_PATH)).await.expect("get").bytes().await.expect("body");
        assert_eq!(body, image);
        assert_eq!(server.image_stats().requests(), 1);
        assert_eq!(server.image_stats().sent_bytes(), 10_000);
        assert_eq!(server.image_stats().completed(), 1);

        let zip =
            reqwest::get(server.url(PACKAGE_PATH)).await.expect("get").bytes().await.expect("body");
        assert_eq!(&zip[..], b"zip");
        assert_eq!(server.package_stats().completed(), 1);
        assert_eq!(server.address().ip().to_string(), "127.0.0.1");

        let missing = reqwest::get(server.url("/other")).await.expect("get");
        assert_eq!(missing.status(), 404);
    }
}
