//! Plain HTTP clients for the devices.
//!
//! reqwest is built with rustls but without a default crypto provider, so
//! that only ring is compiled in. A client without its own TLS configuration
//! needs a process-wide provider, otherwise building it panics; [`builder`]
//! installs ring first. The client for Shelly's servers brings its own
//! configuration (`shelly_tls`).

/// A client builder; ring is installed as the process-wide crypto provider.
pub fn builder() -> reqwest::ClientBuilder {
    // Fails only if a provider is installed already, which is just as good.
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
}

#[cfg(test)]
#[path = "unit_tests/http.rs"]
mod tests;
