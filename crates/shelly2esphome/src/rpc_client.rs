//! The JSON-RPC of Shelly Gen2 and later (`POST /rpc`), with the digest
//! authentication (RFC 7616, SHA-256) the stock firmware asks for when its
//! web authentication is enabled. The user name is always `admin` there.

use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::device::Credentials;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const CNONCE_BYTES: usize = 16;

/// Reasons why an RPC call failed.
#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    /// No usable answer: unreachable, timed out or not JSON.
    #[error("device unreachable or unreadable answer: {0}")]
    Http(#[from] reqwest::Error),
    /// Web authentication is enabled and no login was given.
    #[error("the device requires a login (--auth admin:password)")]
    AuthRequired,
    /// The device refused the login.
    #[error(
        "the device refused the login; Gen2 and later devices use the user admin \
         (--auth admin:password)"
    )]
    AuthFailed,
    /// An HTTP status other than success, without an RPC error.
    #[error("{method} answered with HTTP {status}")]
    Status {
        /// RPC method.
        method: String,
        /// HTTP status.
        status: u16,
    },
    /// A successful answer that is no RPC reply.
    #[error("{0} gave an answer that is no RPC reply")]
    Unreadable(String),
    /// The device refused the call.
    #[error("{method} failed: {message} (code {code})")]
    Refused {
        /// RPC method.
        method: String,
        /// Error code of the device.
        code: i64,
        /// Message of the device.
        message: String,
    },
}

#[derive(Debug, Deserialize)]
struct ErrorWire {
    code: i64,
    message: String,
}

#[derive(Debug, Deserialize)]
struct Reply {
    #[serde(default)]
    result: serde_json::Value,
    error: Option<ErrorWire>,
}

/// Client for the RPC of one device.
#[derive(Debug, Clone)]
pub struct RpcClient {
    url: String,
    http: reqwest::Client,
    credentials: Option<Credentials>,
}

/// The `key=value` and `key="value"` parameters of a `WWW-Authenticate`
/// digest challenge.
fn challenge_parameters(challenge: &str) -> Vec<(String, String)> {
    let mut parameters = Vec::new();
    let mut rest = challenge.trim().strip_prefix("Digest").unwrap_or_default().trim_start();
    while let Some((key, value)) = rest.split_once('=') {
        let key = key.trim().trim_start_matches(',').trim().to_owned();
        let (value, after) = if let Some(quoted) = value.strip_prefix('"') {
            quoted.split_once('"').unwrap_or((quoted, ""))
        } else {
            value.split_once(',').unwrap_or((value, ""))
        };
        parameters.push((key, value.trim().to_owned()));
        rest = after;
    }
    parameters
}

fn sha256_hex(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// The `Authorization` header answering `challenge` for a POST to `uri`, or
/// `None` for a challenge this client cannot answer (not SHA-256, no realm
/// or nonce, a quality of protection other than `auth`).
fn authorization(
    challenge: &str,
    credentials: &Credentials,
    method: &str,
    uri: &str,
    cnonce: &str,
) -> Option<String> {
    let parameters = challenge_parameters(challenge);
    let get = |name: &str| {
        parameters.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value)
    };
    let realm = get("realm")?;
    let nonce = get("nonce")?;
    if !get("algorithm").is_some_and(|algorithm| algorithm.eq_ignore_ascii_case("SHA-256")) {
        return None;
    }
    if !get("qop").is_some_and(|qop| qop.split(',').any(|qop| qop.trim() == "auth")) {
        return None;
    }
    let count = "00000001";
    let user = credentials.user();
    let secret = sha256_hex(&format!("{user}:{realm}:{}", credentials.password()));
    let request = sha256_hex(&format!("{method}:{uri}"));
    let response = sha256_hex(&format!("{secret}:{nonce}:{count}:{cnonce}:auth:{request}"));
    let opaque = get("opaque").map(|opaque| format!(", opaque=\"{opaque}\"")).unwrap_or_default();
    Some(format!(
        "Digest username=\"{user}\", realm=\"{realm}\", nonce=\"{nonce}\", uri=\"{uri}\", \
         algorithm=SHA-256, response=\"{response}\", qop=auth, nc={count}, \
         cnonce=\"{cnonce}\"{opaque}"
    ))
}

fn cnonce() -> String {
    let mut random = [0_u8; CNONCE_BYTES];
    // Without an OS random source there is no safe nonce; nothing works then.
    getrandom::fill(&mut random).expect("the operating system's random source is unavailable");
    hex::encode(random)
}

impl RpcClient {
    /// A client for the device at `host` (`ip`, `ip:port` or a name).
    #[must_use]
    pub fn new(host: &str, credentials: Option<Credentials>) -> Self {
        // Without TLS backends, building the client cannot fail.
        let http = crate::http::builder().timeout(REQUEST_TIMEOUT).build().unwrap_or_default();
        Self { url: format!("http://{host}/rpc"), http, credentials }
    }

    /// Calls `method` with `params` and returns its result.
    ///
    /// # Errors
    /// An [`RpcError`] if the device cannot be reached, asks for a login it
    /// did not get or refuses the call.
    pub async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, RpcError> {
        let body = serde_json::json!({ "id": 1, "method": method, "params": params });
        let mut response = self.http.post(&self.url).json(&body).send().await?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            let credentials = self.credentials.as_ref().ok_or(RpcError::AuthRequired)?;
            let challenge = response
                .headers()
                .get(reqwest::header::WWW_AUTHENTICATE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            let header = authorization(challenge, credentials, "POST", "/rpc", &cnonce())
                .ok_or(RpcError::AuthFailed)?;
            response = self
                .http
                .post(&self.url)
                .header(reqwest::header::AUTHORIZATION, header)
                .json(&body)
                .send()
                .await?;
            if response.status() == reqwest::StatusCode::UNAUTHORIZED {
                return Err(RpcError::AuthFailed);
            }
        }
        let status = response.status();
        let text = response.text().await?;
        let refused = |error: ErrorWire| RpcError::Refused {
            method: method.to_owned(),
            code: error.code,
            message: error.message,
        };
        // The device answers an RPC error inside the reply, or on its own
        // with an HTTP error status.
        if let Ok(error) = serde_json::from_str::<ErrorWire>(&text) {
            return Err(refused(error));
        }
        if !status.is_success() {
            return Err(RpcError::Status { method: method.to_owned(), status: status.as_u16() });
        }
        let reply: Reply =
            serde_json::from_str(&text).map_err(|_| RpcError::Unreadable(method.to_owned()))?;
        match reply.error {
            Some(error) => Err(refused(error)),
            None => Ok(reply.result),
        }
    }
}

#[cfg(test)]
#[path = "unit_tests/rpc_client.rs"]
mod tests;
