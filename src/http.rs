use std::io::{ErrorKind, Read, Write};
use std::net::Ipv4Addr;

use crate::Error;
use crate::tls::tls_connect;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HttpResponse {
    pub(crate) status_code: u16,
    pub(crate) body: Vec<u8>,
    pub(crate) etag: Option<String>,
    pub(crate) last_modified: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResponseMetadata {
    header_end: usize,
    status_code: u16,
    content_length: Option<usize>,
    chunked: bool,
    etag: Option<String>,
    last_modified: Option<String>,
}

pub(crate) fn https_get_with_candidates(
    host: &str,
    path: &str,
    headers: &[(String, String)],
    ips: &[Ipv4Addr],
) -> Result<HttpResponse, Error> {
    let mut last_error = None;
    for ip in ips {
        match https_get_via_ip(host, path, headers, *ip) {
            Ok(body) => return Ok(body),
            Err(error) => last_error = Some(error),
        }
    }

    Err(last_error
        .unwrap_or_else(|| Error::Dns(format!("no IPv4 addresses available for '{host}'"))))
}

pub(crate) fn successful_body(response: HttpResponse) -> Result<Vec<u8>, Error> {
    if (200..300).contains(&response.status_code) {
        Ok(response.body)
    } else {
        Err(Error::Http {
            status: response.status_code,
            body: body_text_for_error(&response.body),
        })
    }
}

fn https_get_via_ip(
    host: &str,
    path: &str,
    headers: &[(String, String)],
    ip: Ipv4Addr,
) -> Result<HttpResponse, Error> {
    let mut stream = tls_connect(host, 443, ip)?;
    let request = build_request(host, path, headers);
    stream.write_all(request.as_bytes())?;
    stream.flush()?;

    let mut response = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(read) => {
                response.extend_from_slice(&buf[..read]);
                if let Some(complete_len) = http_response_complete_len(&response)? {
                    response.truncate(complete_len);
                    break;
                }
            }
            Err(error) if error.kind() == ErrorKind::UnexpectedEof && !response.is_empty() => {
                if let Some(complete_len) = http_response_complete_len(&response)? {
                    response.truncate(complete_len);
                    break;
                }
                return Err(error.into());
            }
            Err(error) => return Err(error.into()),
        }
    }
    parse_http_response(&response)
}

fn build_request(host: &str, path: &str, headers: &[(String, String)]) -> String {
    let mut request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nAccept: */*\r\nConnection: close\r\nUser-Agent: vzglyd_sidecar/0.1\r\n"
    );
    for (name, value) in headers {
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    request
}

fn parse_http_response(response: &[u8]) -> Result<HttpResponse, Error> {
    let metadata = http_response_metadata(response)?
        .ok_or_else(|| Error::Io("HTTP response missing header terminator".to_string()))?;

    let body_bytes = &response[metadata.header_end + 4..];
    let body = if metadata.chunked {
        decode_chunked_body(body_bytes)?
    } else if let Some(content_length) = metadata.content_length {
        body_bytes
            .get(..content_length)
            .ok_or_else(|| {
                Error::Io(format!(
                    "HTTP body shorter than declared Content-Length ({content_length})"
                ))
            })?
            .to_vec()
    } else {
        body_bytes.to_vec()
    };

    Ok(HttpResponse {
        status_code: metadata.status_code,
        body,
        etag: metadata.etag,
        last_modified: metadata.last_modified,
    })
}

fn http_response_metadata(response: &[u8]) -> Result<Option<ResponseMetadata>, Error> {
    let Some(header_end) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
        return Ok(None);
    };

    let header_text = std::str::from_utf8(&response[..header_end])
        .map_err(|error| Error::Io(format!("HTTP headers were not valid UTF-8: {error}")))?;
    let mut lines = header_text.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| Error::Io("HTTP response missing status line".to_string()))?;
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| Error::Io("HTTP status line missing code".to_string()))?
        .parse::<u16>()
        .map_err(|error| Error::Io(format!("invalid HTTP status code: {error}")))?;

    let mut content_length = None;
    let mut chunked = false;
    let mut etag = None;
    let mut last_modified = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse::<usize>().ok();
        } else if name.eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase().contains("chunked")
        {
            chunked = true;
        } else if name.eq_ignore_ascii_case("etag") {
            etag = Some(value.to_string());
        } else if name.eq_ignore_ascii_case("last-modified") {
            last_modified = Some(value.to_string());
        }
    }

    Ok(Some(ResponseMetadata {
        header_end,
        status_code,
        content_length,
        chunked,
        etag,
        last_modified,
    }))
}

fn http_response_complete_len(response: &[u8]) -> Result<Option<usize>, Error> {
    let Some(metadata) = http_response_metadata(response)? else {
        return Ok(None);
    };

    let body = &response[metadata.header_end + 4..];
    if metadata.chunked {
        chunked_wire_len(body).map(|body_len| body_len.map(|len| metadata.header_end + 4 + len))
    } else if let Some(content_length) = metadata.content_length {
        if body.len() >= content_length {
            Ok(Some(metadata.header_end + 4 + content_length))
        } else {
            Ok(None)
        }
    } else {
        Ok(None)
    }
}

fn chunked_wire_len(body: &[u8]) -> Result<Option<usize>, Error> {
    let mut cursor = 0usize;
    loop {
        let Some(line_end) = body[cursor..]
            .windows(2)
            .position(|window| window == b"\r\n")
            .map(|offset| cursor + offset)
        else {
            return Ok(None);
        };
        let line = std::str::from_utf8(&body[cursor..line_end])
            .map_err(|error| Error::Io(format!("chunk size line was not valid UTF-8: {error}")))?;
        let size_hex = line.split(';').next().unwrap_or(line).trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|error| Error::Io(format!("invalid chunk size '{size_hex}': {error}")))?;
        cursor = line_end + 2;

        if size == 0 {
            if body.get(cursor..cursor + 2) == Some(b"\r\n") {
                return Ok(Some(cursor + 2));
            }
            return Ok(None);
        }

        let end = cursor
            .checked_add(size)
            .ok_or_else(|| Error::Io("chunk size overflowed response body".to_string()))?;
        if body.get(cursor..end).is_none() {
            return Ok(None);
        }
        cursor = end;

        if body.get(cursor..cursor + 2).is_none() {
            return Ok(None);
        }
        if body.get(cursor..cursor + 2) != Some(b"\r\n") {
            return Err(Error::Io(
                "chunked body missing CRLF after chunk payload".to_string(),
            ));
        }
        cursor += 2;
    }
}

fn decode_chunked_body(body: &[u8]) -> Result<Vec<u8>, Error> {
    let mut cursor = 0usize;
    let mut decoded = Vec::new();

    loop {
        let Some(line_end) = body[cursor..]
            .windows(2)
            .position(|window| window == b"\r\n")
            .map(|offset| cursor + offset)
        else {
            return Err(Error::Io("chunked body missing size delimiter".to_string()));
        };
        let line = std::str::from_utf8(&body[cursor..line_end])
            .map_err(|error| Error::Io(format!("chunk size line was not valid UTF-8: {error}")))?;
        let size_hex = line.split(';').next().unwrap_or(line).trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|error| Error::Io(format!("invalid chunk size '{size_hex}': {error}")))?;
        cursor = line_end + 2;

        if size == 0 {
            return Ok(decoded);
        }

        let end = cursor
            .checked_add(size)
            .ok_or_else(|| Error::Io("chunk size overflowed response body".to_string()))?;
        let chunk = body.get(cursor..end).ok_or_else(|| {
            Error::Io("chunked body ended before chunk payload completed".to_string())
        })?;
        decoded.extend_from_slice(chunk);
        cursor = end;

        if body.get(cursor..cursor + 2) != Some(b"\r\n") {
            return Err(Error::Io(
                "chunked body missing CRLF after chunk payload".to_string(),
            ));
        }
        cursor += 2;
    }
}

fn body_text_for_error(body: &[u8]) -> String {
    String::from_utf8_lossy(body).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_http_response_extracts_content_length_body() {
        let response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nhello world";
        let body = successful_body(parse_http_response(response).expect("parse HTTP response"))
            .expect("extract body");
        assert_eq!(body, b"hello world");
    }

    #[test]
    fn parse_http_response_decodes_chunked_body() {
        let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        let body = successful_body(parse_http_response(response).expect("parse chunked response"))
            .expect("extract body");
        assert_eq!(body, b"hello world");
    }

    #[test]
    fn parse_http_response_extracts_conditional_headers() {
        let response = b"HTTP/1.1 304 Not Modified\r\nETag: \"etag-1\"\r\nLast-Modified: Sun, 01 Jan 2023 00:00:00 GMT\r\nContent-Length: 0\r\n\r\n";
        let response = parse_http_response(response).expect("parse conditional response");
        assert_eq!(response.status_code, 304);
        assert_eq!(response.etag.as_deref(), Some("\"etag-1\""));
        assert_eq!(
            response.last_modified.as_deref(),
            Some("Sun, 01 Jan 2023 00:00:00 GMT")
        );
        assert!(response.body.is_empty());
    }
}
