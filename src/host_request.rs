//! Host-mediated request wire used by sidecars and native hosts.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{Error, perform_get, socket, DNS_RESOLVER};

const WIRE_VERSION: u8 = 1;

/// A single HTTP header in host-mediated requests or responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    /// Header name.
    pub name: String,
    /// Header value.
    pub value: String,
}

/// Request payload sent from the sidecar guest to the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostRequest {
    /// Perform an HTTPS `GET`.
    HttpsGet {
        /// Remote host name.
        host: String,
        /// Absolute request path, including query string.
        path: String,
        /// Optional request headers.
        #[serde(default)]
        headers: Vec<Header>,
    },
    /// Measure TCP reachability and connection latency.
    TcpConnect {
        /// Remote host name.
        host: String,
        /// TCP port to connect to.
        port: u16,
        /// Timeout budget in milliseconds.
        timeout_ms: u32,
    },
}

/// Structured error class carried across the host request wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// DNS resolution failed.
    Dns,
    /// TLS setup or handshake failed.
    Tls,
    /// General I/O failed.
    Io,
    /// The request timed out.
    Timeout,
}

/// Response payload returned by the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostResponse {
    /// HTTP response metadata and body.
    Http {
        /// HTTP status code.
        status_code: u16,
        /// Selected response headers.
        #[serde(default)]
        headers: Vec<Header>,
        /// Response body bytes.
        body: Vec<u8>,
    },
    /// Successful TCP reachability result.
    TcpConnect {
        /// Milliseconds spent establishing the socket.
        elapsed_ms: u64,
    },
    /// Host-side transport failure.
    Error {
        /// Error classification.
        error_kind: ErrorKind,
        /// Human-readable error text.
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct VersionedRequest {
    wire_version: u8,
    #[serde(flatten)]
    payload: HostRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct VersionedResponse {
    wire_version: u8,
    #[serde(flatten)]
    payload: HostResponse,
}

/// Encode a request into the JSON wire format.
pub fn encode_request(request: &HostRequest) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(&VersionedRequest {
        wire_version: WIRE_VERSION,
        payload: request.clone(),
    })
    .map_err(|error| Error::Io(format!("failed to encode host request: {error}")))
}

/// Decode a request from the JSON wire format.
pub fn decode_request(bytes: &[u8]) -> Result<HostRequest, Error> {
    let request: VersionedRequest = serde_json::from_slice(bytes)
        .map_err(|error| Error::Io(format!("failed to decode host request: {error}")))?;
    if request.wire_version != WIRE_VERSION {
        return Err(Error::Io(format!(
            "unsupported host request wire version {}",
            request.wire_version
        )));
    }
    Ok(request.payload)
}

/// Encode a response into the JSON wire format.
pub fn encode_response(response: &HostResponse) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(&VersionedResponse {
        wire_version: WIRE_VERSION,
        payload: response.clone(),
    })
    .map_err(|error| Error::Io(format!("failed to encode host response: {error}")))
}

/// Decode a response from the JSON wire format.
pub fn decode_response(bytes: &[u8]) -> Result<HostResponse, Error> {
    let response: VersionedResponse = serde_json::from_slice(bytes)
        .map_err(|error| Error::Io(format!("failed to decode host response: {error}")))?;
    if response.wire_version != WIRE_VERSION {
        return Err(Error::Io(format!(
            "unsupported host response wire version {}",
            response.wire_version
        )));
    }
    Ok(response.payload)
}

/// Execute a request on the host and return the structured response.
pub fn execute_request(request: HostRequest) -> HostResponse {
    match request {
        HostRequest::HttpsGet {
            host,
            path,
            headers,
        } => match perform_get(&host, &path, &headers_as_pairs(&headers)) {
            Ok(response) => HostResponse::Http {
                status_code: response.status_code,
                headers: headers_from_response(&response),
                body: response.body,
            },
            Err(error) => error_response(error),
        },
        HostRequest::TcpConnect {
            host,
            port,
            timeout_ms,
        } => match connect_duration(&host, port, timeout_ms) {
            Ok(elapsed) => HostResponse::TcpConnect {
                elapsed_ms: elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
            },
            Err(error) => error_response(error),
        },
    }
}

/// Decode request bytes, execute them, and encode the response.
pub fn execute_request_bytes(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let request = decode_request(bytes)?;
    encode_response(&execute_request(request))
}

/// Convert a host transport error into the public sidecar error type.
pub fn decode_error(kind: ErrorKind, message: String) -> Error {
    match kind {
        ErrorKind::Dns => Error::Dns(message),
        ErrorKind::Tls => Error::Tls(message),
        ErrorKind::Io => Error::Io(message),
        ErrorKind::Timeout => Error::Timeout,
    }
}

fn headers_as_pairs(headers: &[Header]) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|header| (header.name.clone(), header.value.clone()))
        .collect()
}

fn headers_from_response(response: &crate::http::HttpResponse) -> Vec<Header> {
    let mut headers = Vec::new();
    if let Some(etag) = &response.etag {
        headers.push(Header {
            name: "etag".to_string(),
            value: etag.clone(),
        });
    }
    if let Some(last_modified) = &response.last_modified {
        headers.push(Header {
            name: "last-modified".to_string(),
            value: last_modified.clone(),
        });
    }
    headers
}

fn connect_duration(host: &str, port: u16, timeout_ms: u32) -> Result<Duration, Error> {
    let addrs = DNS_RESOLVER.with(|resolver| resolver.borrow_mut().resolve(host))?;
    socket::connect_any(&addrs, port, Duration::from_millis(u64::from(timeout_ms)))
}

fn error_response(error: Error) -> HostResponse {
    HostResponse::Error {
        error_kind: match error {
            Error::Dns(_) => ErrorKind::Dns,
            Error::Tls(_) => ErrorKind::Tls,
            Error::Io(_) | Error::Http { .. } => ErrorKind::Io,
            Error::Timeout => ErrorKind::Timeout,
        },
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_wire_roundtrips() {
        let request = HostRequest::HttpsGet {
            host: "air-quality-api.open-meteo.com".to_string(),
            path: "/v1/air-quality".to_string(),
            headers: vec![Header {
                name: "accept".to_string(),
                value: "application/json".to_string(),
            }],
        };

        let encoded = encode_request(&request).expect("encode request");
        let decoded = decode_request(&encoded).expect("decode request");
        assert_eq!(decoded, request);
    }

    #[test]
    fn response_wire_roundtrips() {
        let response = HostResponse::Http {
            status_code: 200,
            headers: vec![Header {
                name: "etag".to_string(),
                value: "\"abc\"".to_string(),
            }],
            body: b"payload".to_vec(),
        };

        let encoded = encode_response(&response).expect("encode response");
        let decoded = decode_response(&encoded).expect("decode response");
        assert_eq!(decoded, response);
    }
}
