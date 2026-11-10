//! Unit tests of `crate::server` (not counted as production code for coverage).

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
    let (first, second) =
        tokio::join!(reqwest::get(server.url(IMAGE_PATH)), reqwest::get(server.url(IMAGE_PATH)));
    let (first, second) = (first.expect("first"), second.expect("second"));
    assert_eq!(first.bytes().await.expect("body").len(), 10_000);
    assert_eq!(second.bytes().await.expect("body").len(), 10_000);
    assert_eq!(server.image_stats().completed(), 2);
    assert_eq!(server.image_stats().requests(), 2);
    assert_eq!(server.image_stats().sent_bytes(), 10_000);
}

#[test]
fn only_the_newest_response_reports_progress() {
    let stats = Arc::new(TransferStats::default());
    stats.requests.store(2, Ordering::SeqCst);
    let mut older =
        ResponseProgress { stats: Arc::clone(&stats), request_id: 1, sent: 0, total: 8 };
    let mut newer =
        ResponseProgress { stats: Arc::clone(&stats), request_id: 2, sent: 0, total: 8 };
    newer.record(4);
    older.record(4);
    assert_eq!(stats.sent_bytes(), 4);
    older.record(4);
    assert_eq!(stats.sent_bytes(), 4, "the older response must not report");
    assert_eq!(stats.completed(), 1);
    newer.record(4);
    assert_eq!(stats.sent_bytes(), 8);
    assert_eq!(stats.completed(), 2);
}
