#![deny(missing_docs)]
//! # `vzglyd_sidecar`
//!
//! Networking and IPC utilities for [VZGLYD](https://github.com/vzglyd/vzglyd) slide sidecars.
//!
//! A sidecar is a companion `wasm32-wasip1` program that fetches external data and pushes it
//! into a paired slide over the VZGLYD host channel.
//!
//! ## Typical Structure
//!
//! ```no_run
//! use vzglyd_sidecar::{https_get_text, poll_loop};
//!
//! fn main() {
//!     poll_loop(60, || {
//!         let body = https_get_text("api.example.com", "/data")?;
//!         Ok(body.into_bytes())
//!     });
//! }
//! ```
//!
//! This crate is primarily intended for the `wasm32-wasip1` target used by VZGLYD sidecars.

mod channel;
mod dns;
pub mod host_request;
mod http;
mod poll;
mod socket;
mod tls;

use std::cell::RefCell;
use std::fmt;

pub use channel::{channel_active, channel_poll, channel_push, info_log, sleep_secs};
pub use host_request::{Header as HostHeader, HostRequest, HostResponse};
pub use poll::poll_loop;
use std::time::Duration;

thread_local! {
    static DNS_RESOLVER: RefCell<dns::DnsResolver> = RefCell::new(dns::DnsResolver::new());
}

/// Errors returned by network and channel helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// DNS resolution failed.
    Dns(String),
    /// TLS handshake or certificate validation failed.
    Tls(String),
    /// The server responded with an HTTP error status.
    Http {
        /// HTTP status code returned by the server.
        status: u16,
        /// Response body returned with the error status.
        body: String,
    },
    /// General I/O error.
    Io(String),
    /// The operation timed out.
    Timeout,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dns(message) => write!(f, "DNS error: {message}"),
            Self::Tls(message) => write!(f, "TLS error: {message}"),
            Self::Http { status, body } => write!(f, "HTTP {status}: {body}"),
            Self::Io(message) => write!(f, "I/O error: {message}"),
            Self::Timeout => f.write_str("operation timed out"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        if error.kind() == std::io::ErrorKind::TimedOut {
            Self::Timeout
        } else {
            Self::Io(error.to_string())
        }
    }
}

/// Perform an HTTPS `GET` request and return the raw response body.
///
/// # Errors
///
/// Returns [`Error`] if the host-mediated request or HTTP response handling fails.
pub fn https_get(host: &str, path: &str) -> Result<Vec<u8>, Error> {
    let response = execute_https_get(host, path, &[])?;
    http::successful_body(response)
}

/// Perform an HTTPS `GET` request and decode the body as UTF-8 text.
///
/// # Errors
///
/// Returns [`Error`] if the request fails or the response body is not valid UTF-8.
pub fn https_get_text(host: &str, path: &str) -> Result<String, Error> {
    let body = https_get(host, path)?;
    String::from_utf8(body)
        .map_err(|error| Error::Io(format!("HTTP body was not valid UTF-8: {error}")))
}

/// Body, ETag, and Last-Modified returned by a conditional GET.
pub type ConditionalGetResult = Result<(Vec<u8>, Option<String>, Option<String>), Error>;

/// Perform a conditional HTTPS `GET` request using `ETag` and `Last-Modified` hints.
///
/// When the server responds with `304 Not Modified`, the returned body is empty and the cached
/// validators are preserved.
///
/// # Errors
///
/// Returns [`Error`] if the host-mediated request or HTTP response handling fails.
pub fn https_get_conditional(
    host: &str,
    path: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> ConditionalGetResult {
    let mut headers = Vec::new();
    if let Some(etag) = etag {
        headers.push(("If-None-Match".to_string(), etag.to_string()));
    }
    if let Some(last_modified) = last_modified {
        headers.push(("If-Modified-Since".to_string(), last_modified.to_string()));
    }

    let response = execute_https_get(host, path, &headers)?;
    if response.status_code == 304 {
        return Ok((
            Vec::new(),
            response.etag.or_else(|| etag.map(ToOwned::to_owned)),
            response
                .last_modified
                .or_else(|| last_modified.map(ToOwned::to_owned)),
        ));
    }

    let etag = response.etag.clone();
    let last_modified = response.last_modified.clone();
    let body = http::successful_body(response)?;
    Ok((body, etag, last_modified))
}

/// Read an environment variable from the sidecar process.
pub fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Attempt a TCP connection and return the time taken to establish it.
///
/// This is primarily useful for health-check sidecars that want to measure reachability or
/// approximate latency.
///
/// # Errors
///
/// Returns [`Error`] if DNS resolution fails or no socket can be connected before the timeout.
pub fn tcp_connect(host: &str, port: u16, timeout_ms: u32) -> Result<Duration, Error> {
    match execute_host_request(HostRequest::TcpConnect {
        host: host.to_string(),
        port,
        timeout_ms,
    })? {
        HostResponse::TcpConnect { elapsed_ms } => Ok(Duration::from_millis(elapsed_ms)),
        HostResponse::Error {
            error_kind,
            message,
        } => Err(host_request::decode_error(error_kind, message)),
        HostResponse::Http { .. } => Err(Error::Io(
            "host returned an HTTP response for tcp_connect".to_string(),
        )),
    }
}

/// Split an HTTPS URL into `(host, path)` for use with [`https_get`] helpers.
///
/// # Errors
///
/// Returns [`Error`] if the URL is not HTTPS or does not contain a valid host.
pub fn split_https_url(url: &str) -> Result<(String, String), Error> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| Error::Io(format!("unsupported URL scheme in '{url}'")))?;

    if rest.is_empty() {
        return Err(Error::Io("HTTPS URL is missing a host".to_string()));
    }

    let (host, path) = if let Some((host, remainder)) = rest.split_once('/') {
        (host, format!("/{}", remainder))
    } else if let Some((host, query)) = rest.split_once('?') {
        (host, format!("/?{query}"))
    } else {
        (rest, "/".to_string())
    };

    if host.is_empty() {
        return Err(Error::Io(format!("HTTPS URL is missing a host in '{url}'")));
    }

    Ok((host.to_string(), path))
}

fn perform_get(
    host: &str,
    path: &str,
    headers: &[(String, String)],
) -> Result<http::HttpResponse, Error> {
    let addrs = DNS_RESOLVER.with(|resolver| resolver.borrow_mut().resolve(host))?;
    http::https_get_with_candidates(host, path, headers, &addrs)
}

fn execute_https_get(
    host: &str,
    path: &str,
    headers: &[(String, String)],
) -> Result<http::HttpResponse, Error> {
    match execute_host_request(HostRequest::HttpsGet {
        host: host.to_string(),
        path: path.to_string(),
        headers: headers
            .iter()
            .map(|(name, value)| HostHeader {
                name: name.clone(),
                value: value.clone(),
            })
            .collect(),
    })? {
        HostResponse::Http {
            status_code,
            headers,
            body,
        } => Ok(http::HttpResponse {
            status_code,
            etag: header_value(&headers, "etag").map(ToOwned::to_owned),
            last_modified: header_value(&headers, "last-modified").map(ToOwned::to_owned),
            body,
        }),
        HostResponse::Error {
            error_kind,
            message,
        } => Err(host_request::decode_error(error_kind, message)),
        HostResponse::TcpConnect { .. } => Err(Error::Io(
            "host returned a tcp_connect response for an HTTPS request".to_string(),
        )),
    }
}

fn execute_host_request(request: HostRequest) -> Result<HostResponse, Error> {
    #[cfg(target_arch = "wasm32")]
    {
        let request_bytes = host_request::encode_request(&request)?;
        let response_bytes = channel::network_roundtrip(&request_bytes)?;
        host_request::decode_response(&response_bytes)
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        Ok(host_request::execute_request(request))
    }
}

fn header_value<'a>(headers: &'a [HostHeader], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires outbound network"]
    fn https_get_and_https_get_text_fetch_public_https_payloads() {
        let text_body = https_get_text("api.coinbase.com", "/v2/prices/BTC-USD/spot")
            .expect("fetch coinbase text payload");
        assert!(text_body.contains("\"amount\""));

        let bytes_body =
            https_get("api.coinbase.com", "/v2/prices/BTC-USD/spot").expect("fetch bytes payload");
        let bytes_text =
            std::str::from_utf8(&bytes_body).expect("coinbase payload should be UTF-8");
        assert!(bytes_text.contains("\"amount\""));
    }

    #[test]
    #[ignore = "requires outbound network"]
    fn https_get_conditional_reuses_etag_for_not_modified_responses() {
        let (body, etag, last_modified) = https_get_conditional("api.github.com", "/", None, None)
            .expect("fetch github metadata");
        assert!(!body.is_empty());
        assert!(etag.is_some() || last_modified.is_some());

        let (cached_body, cached_etag, cached_last_modified) = https_get_conditional(
            "api.github.com",
            "/",
            etag.as_deref(),
            last_modified.as_deref(),
        )
        .expect("perform conditional github request");
        assert!(cached_body.is_empty(), "expected 304 body to be empty");
        assert!(cached_etag.is_some() || cached_last_modified.is_some());
    }

    #[test]
    fn split_https_url_handles_plain_and_query_urls() {
        assert_eq!(
            split_https_url("https://calendar.google.com/calendar/ical/test/basic.ics").unwrap(),
            (
                "calendar.google.com".to_string(),
                "/calendar/ical/test/basic.ics".to_string()
            )
        );
        assert_eq!(
            split_https_url("https://example.com?foo=bar").unwrap(),
            ("example.com".to_string(), "/?foo=bar".to_string())
        );
    }
}
