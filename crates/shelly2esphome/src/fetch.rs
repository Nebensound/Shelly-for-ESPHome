//! The official package of a Gen2+ device from Shelly's update service.
//!
//! `https://updates.shelly.cloud/update/<app>` names the current stable
//! release: its version and a URL on `fwcdn.shelly.cloud` whose last path
//! segment is the SHA-256 of the package. Both servers are reached through
//! [`crate::shelly_tls`]. A package is used only if its SHA-256 is the
//! announced one, it passes [`StockPackage::parse`] and it names the
//! requested application and the announced version.
//!
//! Packages are kept in the cache directory as `<app>/<version>.zip` and
//! checked the same way on reuse; a cached file with another SHA-256 is
//! replaced. The update service is asked every time, so the cache saves the
//! download, not the lookup.

use std::{
    error::Error as _,
    io::{Read, Write},
    path::{Path, PathBuf},
    str::FromStr,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{
    rpc::{AppName, FirmwareVersion},
    shelly_tls::{self, TlsError},
    stock_package::{MAX_PACKAGE_BYTES, StockPackage, StockPackageError},
};

/// The update service; the application name is appended.
pub const UPDATE_API: &str = "https://updates.shelly.cloud/update/";
/// Largest reply of the update service this tool reads.
pub const MAX_REPLY_BYTES: usize = 64 * 1024;

/// Where and how packages are fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchOptions {
    /// Root certificate (DER) the servers' certificates must be signed by.
    pub root: Vec<u8>,
    /// Host names resolved to fixed addresses instead of DNS (tests). With
    /// any of them, the client connects directly, without a proxy from the
    /// environment, which would bypass them.
    pub resolve: Vec<(String, std::net::SocketAddr)>,
    /// Cache directory; `None` downloads every time.
    pub cache: Option<PathBuf>,
    /// Timeout for connecting and for each read.
    pub timeout: Duration,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            root: shelly_tls::ROOT.to_vec(),
            resolve: Vec::new(),
            cache: dirs::cache_dir().map(|dir| dir.join("shelly2esphome").join("packages")),
            timeout: Duration::from_secs(30),
        }
    }
}

/// Why no official package could be fetched. Every network failure names
/// the URL.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The application name cannot name a package.
    #[error("{0}")]
    AppName(String),
    /// The HTTPS client cannot be built.
    #[error(transparent)]
    Client(TlsError),
    /// The request failed (connection, TLS, HTTP status, cut-off body).
    #[error("{url}: {reason}")]
    Request {
        /// Requested URL.
        url: String,
        /// Cause, with its whole chain.
        reason: String,
    },
    /// The reply is larger than this tool reads.
    #[error("{url}: the reply is larger than {limit} bytes")]
    TooLarge {
        /// Requested URL.
        url: String,
        /// The limit in bytes.
        limit: usize,
    },
    /// The update service's reply is unusable.
    #[error("{url}: unexpected reply: {reason}")]
    Reply {
        /// Requested URL.
        url: String,
        /// What is wrong.
        reason: String,
    },
    /// The downloaded package is not the announced one.
    #[error("{url}: the package has SHA-256 {actual}, the update service announced {expected}")]
    Digest {
        /// Package URL.
        url: String,
        /// Announced SHA-256.
        expected: String,
        /// SHA-256 of the received bytes.
        actual: String,
    },
    /// The package fails the validation.
    #[error("{url}: {source}")]
    Package {
        /// Package URL.
        url: String,
        /// Cause.
        source: StockPackageError,
    },
    /// The package is for another application or version.
    #[error("{url}: the package is {found}, the update service announced {announced}")]
    Mismatch {
        /// Package URL.
        url: String,
        /// Application and version in the package.
        found: String,
        /// Requested application and announced version.
        announced: String,
    },
    /// The cache cannot be read or written.
    #[error("package cache {}: {reason}", path.display())]
    Cache {
        /// File or directory.
        path: PathBuf,
        /// Cause.
        reason: String,
    },
}

/// Where a package came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Downloaded now, and kept in the cache file if there is one.
    Downloaded {
        /// Package URL.
        url: String,
        /// Cache file it was written to.
        cached: Option<PathBuf>,
    },
    /// Taken from this cache file.
    Cache(PathBuf),
}

/// A verified official package.
#[derive(Debug, Clone)]
pub struct Fetched {
    /// The package.
    pub package: StockPackage,
    /// Where it came from.
    pub source: Source,
}

#[derive(Debug, Deserialize)]
struct Reply {
    stable: Release,
}

#[derive(Debug, Deserialize)]
struct Release {
    version: FirmwareVersion,
    url: String,
}

/// The cause of a failed request with its whole chain, without the URL
/// (the error message names it once).
fn reason(error: reqwest::Error) -> String {
    let error = error.without_url();
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text = format!("{text}: {cause}");
        source = cause.source();
    }
    text
}

/// GETs `url` and reads at most `limit` bytes of the body.
async fn get(client: &reqwest::Client, url: &str, limit: usize) -> Result<Vec<u8>, FetchError> {
    let failed = |error| FetchError::Request { url: url.to_owned(), reason: reason(error) };
    let mut response = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(failed)?;
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(failed)? {
        body.extend_from_slice(&chunk);
        if body.len() > limit {
            return Err(FetchError::TooLarge { url: url.to_owned(), limit });
        }
    }
    Ok(body)
}

/// Checks that `bytes` are the announced package of `app`.
fn verify(
    bytes: &[u8],
    app: &AppName,
    release: &Release,
    digest: &[u8; 32],
) -> Result<StockPackage, FetchError> {
    let url = &release.url;
    let actual = Sha256::digest(bytes);
    if actual.as_slice() != digest {
        return Err(FetchError::Digest {
            url: url.clone(),
            expected: hex::encode(digest),
            actual: hex::encode(actual),
        });
    }
    let package = StockPackage::parse(bytes)
        .map_err(|source| FetchError::Package { url: url.clone(), source })?;
    if package.name() != app || package.version() != &release.version {
        return Err(FetchError::Mismatch {
            url: url.clone(),
            found: format!("{} {}", package.name(), package.version()),
            announced: format!("{app} {}", release.version),
        });
    }
    Ok(package)
}

/// The cached package of `release`, if the cache holds it with the announced
/// SHA-256. Reads at most one byte more than a package may have, so an
/// oversized file is a miss instead of being read whole.
fn cached(path: &Path, digest: &[u8; 32]) -> Result<Option<Vec<u8>>, FetchError> {
    let limit = u64::try_from(MAX_PACKAGE_BYTES).unwrap_or(u64::MAX).saturating_add(1);
    let read = std::fs::File::open(path).and_then(|file| {
        let mut bytes = Vec::new();
        file.take(limit).read_to_end(&mut bytes).map(|_| bytes)
    });
    match read {
        Ok(bytes) => Ok(Some(bytes).filter(|bytes| {
            bytes.len() <= MAX_PACKAGE_BYTES && Sha256::digest(bytes).as_slice() == digest
        })),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(FetchError::Cache { path: path.to_owned(), reason: error.to_string() }),
    }
}

/// Writes the package to `path` through a temporary file of its own, so
/// that neither an interrupted write nor a concurrent one (another process,
/// or another device in the same run) leaves a partial package under the
/// final name.
fn store(path: &Path, bytes: &[u8]) -> Result<(), FetchError> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let unique = format!("{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
    let partial = path.with_extension(format!("zip.{unique}.part"));
    path.parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::OpenOptions::new().write(true).create_new(true).open(&partial))
        .and_then(|mut file| file.write_all(bytes))
        .and_then(|()| std::fs::rename(&partial, path))
        .map_err(|error| {
            // Nothing is left behind; the file may not even exist.
            let _ = std::fs::remove_file(&partial);
            FetchError::Cache { path: path.to_owned(), reason: error.to_string() }
        })
}

/// Fetches the current stable official package of `app`.
///
/// # Errors
/// Returns a [`FetchError`] naming the URL or cache file and the reason.
pub async fn fetch(app: &AppName, options: &FetchOptions) -> Result<Fetched, FetchError> {
    // The name becomes part of a URL path and of a cache path.
    AppName::from_str(app.as_str()).map_err(FetchError::AppName)?;
    let builder = options.resolve.iter().fold(
        reqwest::Client::builder()
            .user_agent(concat!("shelly2esphome/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(options.timeout)
            .read_timeout(options.timeout),
        |builder, (host, addr)| builder.no_proxy().resolve(host, *addr),
    );
    let client =
        shelly_tls::client_with_root(&options.root, builder).map_err(FetchError::Client)?;

    let api = format!("{UPDATE_API}{app}");
    let unexpected = |reason: String| FetchError::Reply { url: api.clone(), reason };
    let reply = get(&client, &api, MAX_REPLY_BYTES).await?;
    let release = serde_json::from_slice::<Reply>(&reply)
        .map_err(|error| unexpected(error.to_string()))?
        .stable;
    let digest = release
        .url
        .rsplit_once('/')
        .and_then(|(_, last)| {
            let mut digest = [0; 32];
            hex::decode_to_slice(last, &mut digest).ok().map(|()| digest)
        })
        .ok_or_else(|| unexpected(format!("the package URL {} has no SHA-256", release.url)))?;
    let version = release.version.as_str();
    if version.is_empty()
        || version.starts_with('.')
        || !version.bytes().all(|byte| byte.is_ascii_alphanumeric() || b".+-_".contains(&byte))
    {
        return Err(unexpected(format!("implausible version {version:?}")));
    }

    let file =
        options.cache.as_ref().map(|dir| dir.join(app.as_str()).join(format!("{version}.zip")));
    if let Some(path) = &file
        && let Some(bytes) = cached(path, &digest)?
    {
        let package = verify(&bytes, app, &release, &digest)?;
        return Ok(Fetched { package, source: Source::Cache(path.clone()) });
    }
    let bytes = get(&client, &release.url, MAX_PACKAGE_BYTES).await?;
    let package = verify(&bytes, app, &release, &digest)?;
    if let Some(path) = &file {
        store(path, &bytes)?;
    }
    Ok(Fetched { package, source: Source::Downloaded { url: release.url, cached: file } })
}
