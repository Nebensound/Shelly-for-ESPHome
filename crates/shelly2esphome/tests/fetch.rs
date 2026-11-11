//! Fetching the official Gen2+ package from a simulated Shelly cloud.
#![allow(missing_docs, clippy::unwrap_used)]

mod common;

use std::path::PathBuf;

use common::{cloud::*, scratch_dir};
use shelly2esphome::{
    esp32::synthetic::Fixture,
    fetch::{FetchError, FetchOptions, MAX_REPLY_BYTES, Source, UPDATE_API, fetch},
    rpc::AppName,
    shelly_tls::{self, synthetic::TestCa},
    stock_package::MAX_PACKAGE_BYTES,
};

fn s1g3() -> AppName {
    "S1G3".parse().unwrap()
}

async fn error(cloud: &Cloud, cache: Option<PathBuf>) -> String {
    fetch(&s1g3(), &cloud.options(cache)).await.expect_err("must fail").to_string()
}

#[tokio::test]
async fn downloads_verifies_and_caches_the_package() {
    let cloud = Cloud::start().await;
    let package = Fixture::default().package();
    let url = cloud.publish("S1G3", "2.0.3", &package);
    let cache = scratch_dir();
    let file = cache.join("S1G3").join("2.0.3.zip");

    let first = fetch(&s1g3(), &cloud.options(Some(cache.clone()))).await.unwrap();
    assert_eq!(first.package.name().as_str(), "S1G3");
    assert_eq!(first.package.version().as_str(), "2.0.3");
    assert_eq!(first.source, Source::Downloaded { url: url.clone(), cached: Some(file.clone()) });
    assert_eq!(std::fs::read(&file).unwrap(), package);

    // The update service is asked again, the package comes from the cache.
    let second = fetch(&s1g3(), &cloud.options(Some(cache.clone()))).await.unwrap();
    assert_eq!(second.source, Source::Cache(file.clone()));
    let path = url.trim_start_matches("https://fwcdn.shelly.cloud");
    assert_eq!(cloud.requests(), ["/update/S1G3", path, "/update/S1G3"]);

    // A cached file that is not the announced package is replaced.
    std::fs::write(&file, b"stale").unwrap();
    let third = fetch(&s1g3(), &cloud.options(Some(cache))).await.unwrap();
    assert!(matches!(third.source, Source::Downloaded { .. }));
    assert_eq!(std::fs::read(&file).unwrap(), package);

    let uncached = fetch(&s1g3(), &cloud.options(None)).await.unwrap();
    assert_eq!(uncached.source, Source::Downloaded { url, cached: None });
}

#[tokio::test]
async fn refuses_packages_that_are_not_the_announced_one() {
    let cloud = Cloud::start().await;
    let package = Fixture::default().package();
    let url = cloud.publish("S1G3", "2.0.3", &package);
    // Other bytes under the announced digest.
    cloud.route(url.trim_start_matches("https://fwcdn.shelly.cloud"), ok(b"forged"));
    let message = error(&cloud, None).await;
    assert!(message.starts_with(&format!("{url}: the package has SHA-256 ")), "{message}");
    assert!(message.ends_with(&format!("the update service announced {}", &url[url.len() - 64..])));

    // The announced bytes, but no package.
    let junk = cloud.publish("S1G3", "2.0.3", b"junk");
    assert!(
        error(&cloud, None).await.starts_with(&format!("{junk}: the package is no readable ZIP"))
    );
    // The same bytes in the cache are checked just as strictly.
    let cache = scratch_dir();
    std::fs::create_dir_all(cache.join("S1G3")).unwrap();
    std::fs::write(cache.join("S1G3").join("2.0.3.zip"), b"junk").unwrap();
    let requests = cloud.requests().len();
    assert!(
        error(&cloud, Some(cache))
            .await
            .starts_with(&format!("{junk}: the package is no readable ZIP"))
    );
    assert_eq!(cloud.requests().len(), requests + 1, "only the update service is asked");

    // A valid package of another version.
    let other = cloud.publish("S1G3", "2.0.4", &package);
    assert_eq!(
        error(&cloud, None).await,
        format!("{other}: the package is S1G3 2.0.3, the update service announced S1G3 2.0.4")
    );
}

#[tokio::test]
async fn names_the_url_of_every_failed_request() {
    let cloud = Cloud::start().await;
    let api = format!("{UPDATE_API}S1G3");
    assert_eq!(
        error(&cloud, None).await,
        format!("{api}: HTTP status client error (404 Not Found)")
    );

    cloud.route("/update/S1G3", ok(&vec![b' '; MAX_REPLY_BYTES + 1]));
    assert_eq!(
        error(&cloud, None).await,
        format!("{api}: the reply is larger than {MAX_REPLY_BYTES} bytes")
    );

    let package = Fixture::default().package();
    let url = cloud.publish("S1G3", "2.0.3", &package);
    let path = url.trim_start_matches("https://fwcdn.shelly.cloud");
    cloud.route(path, truncated(&package[..100], package.len()));
    let message = error(&cloud, None).await;
    assert!(message.starts_with(&format!("{url}: error decoding response body")), "{message}");

    cloud.route(path, ok(&vec![0; MAX_PACKAGE_BYTES + 1]));
    assert_eq!(
        error(&cloud, None).await,
        format!("{url}: the reply is larger than {MAX_PACKAGE_BYTES} bytes")
    );

    // A server whose certificate is not signed by the trusted root.
    let stranger = FetchOptions { root: TestCa::new(9, "Allterco").root(), ..cloud.options(None) };
    let message = fetch(&s1g3(), &stranger).await.unwrap_err().to_string();
    assert!(message.starts_with(&format!("{api}: error sending request")), "{message}");
    assert!(message.contains("a signature of the server does not match"), "{message}");
}

#[tokio::test]
async fn refuses_unusable_replies() {
    let cloud = Cloud::start().await;
    let api = format!("{UPDATE_API}S1G3");
    cloud.route("/update/S1G3", ok(b"<html>"));
    assert!(
        error(&cloud, None).await.starts_with(&format!("{api}: unexpected reply: expected value"))
    );

    cloud.route("/update/S1G3", ok(br#"{"beta":{}}"#));
    assert!(error(&cloud, None).await.contains("missing field `stable`"));

    cloud.release("S1G3", "2.0.3", "https://fwcdn.shelly.cloud/gen2/S1G3/latest");
    assert_eq!(
        error(&cloud, None).await,
        format!(
            "{api}: unexpected reply: the package URL \
             https://fwcdn.shelly.cloud/gen2/S1G3/latest has no SHA-256"
        )
    );

    let hash = "0".repeat(64);
    for version in ["", ".hidden", "../2.0.3", "2.0.3/x"] {
        cloud.release("S1G3", version, &format!("https://fwcdn.shelly.cloud/gen2/S1G3/{hash}"));
        assert_eq!(
            error(&cloud, None).await,
            format!("{api}: unexpected reply: implausible version {version:?}")
        );
    }
}

#[tokio::test]
async fn reports_cache_failures() {
    let cloud = Cloud::start().await;
    let package = Fixture::default().package();
    cloud.publish("S1G3", "2.0.3", &package);

    // The cache file cannot be read.
    let unreadable = scratch_dir();
    let file = unreadable.join("S1G3").join("2.0.3.zip");
    std::fs::create_dir_all(&file).unwrap();
    let message = error(&cloud, Some(unreadable)).await;
    assert!(message.starts_with(&format!("package cache {}: ", file.display())), "{message}");

    // The package cannot be written (its temporary file is a directory).
    let unwritable = scratch_dir();
    let file = unwritable.join("S1G3").join("2.0.3.zip");
    std::fs::create_dir_all(unwritable.join("S1G3").join("2.0.3.zip.part")).unwrap();
    let message = error(&cloud, Some(unwritable)).await;
    assert!(message.starts_with(&format!("package cache {}: ", file.display())), "{message}");
}

#[tokio::test]
async fn refuses_unusable_names_and_roots() {
    let cloud = Cloud::start().await;
    let device_reported: AppName = serde_json::from_str("\"../S1G3\"").unwrap();
    let message = fetch(&device_reported, &cloud.options(None)).await.unwrap_err().to_string();
    assert_eq!(message, "\"../S1G3\" is no application name (letters and digits, e.g. S1G3)");

    let broken = FetchOptions { root: b"junk".to_vec(), ..cloud.options(None) };
    let error = fetch(&s1g3(), &broken).await.unwrap_err();
    assert!(matches!(error, FetchError::Client(_)));
    assert_eq!(error.to_string(), "the root certificate is malformed");
    assert!(cloud.requests().is_empty());

    let defaults = FetchOptions::default();
    assert_eq!(defaults.root, shelly_tls::ROOT);
    assert!(defaults.resolve.is_empty());
    assert!(defaults.cache.is_some_and(|cache| cache.ends_with("shelly2esphome/packages")));
    assert!("".parse::<AppName>().is_err());
}
