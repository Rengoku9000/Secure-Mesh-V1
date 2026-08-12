//! A deliberately minimal HTTP client that can only reach this machine.
//!
//! # Why not an HTTP crate
//!
//! SecureMesh documents, and verifies, that no HTTP client appears in its
//! dependency tree — it is part of how "no cloud services" is demonstrated
//! rather than asserted (`README.md`, `SECURITY.md` §5.11). Adding `reqwest` or
//! `ureq` to talk to a local model server would make that claim false, and the
//! replacement claim — "an HTTP client is present but we only point it at
//! localhost" — is a promise about intent rather than a property of the build.
//!
//! This client instead takes [`Ipv4Addr::LOCALHOST`] as a constant. It has no
//! DNS resolution, no URL parsing, no proxy support, no TLS, and no way to
//! express a remote host: reaching the Internet with it is not forbidden, it is
//! **unrepresentable**.
//!
//! It speaks just enough HTTP/1.1 to POST JSON to a supervised child process
//! and read the reply. The server it talks to is one SecureMesh started itself,
//! but the response is still parsed defensively and bounded, because a wedged
//! or malfunctioning runtime should produce an error rather than exhaust memory.

use crate::error::{CoreError, CoreResult};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::time::Duration;

/// Largest response body accepted, in bytes.
///
/// Generous for model output, bounded so a runaway runtime cannot exhaust
/// memory.
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// Largest header section accepted.
const MAX_HEADER_BYTES: usize = 64 * 1024;

/// POSTs a JSON body to `127.0.0.1:port` and returns the response body.
///
/// The host is not a parameter. That is the point.
pub fn post_json(port: u16, path: &str, body: &str, timeout: Duration) -> CoreResult<String> {
    request("POST", port, path, Some(body), timeout)
}

/// GETs from `127.0.0.1:port`.
///
/// Needed because readiness endpoints are conventionally GET — llama.cpp's
/// `/health` returns 404 for a POST, which would make a health probe fail
/// permanently and be mistaken for a runtime that never starts.
pub fn get(port: u16, path: &str, timeout: Duration) -> CoreResult<String> {
    request("GET", port, path, None, timeout)
}

fn request(
    method: &str,
    port: u16,
    path: &str,
    body: Option<&str>,
    timeout: Duration,
) -> CoreResult<String> {
    if !path.starts_with('/') {
        return Err(CoreError::internal("request path must be absolute"));
    }

    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    let mut stream = TcpStream::connect_timeout(&address.into(), timeout)
        .map_err(|e| CoreError::internal(format!("local runtime unreachable ({})", e.kind())))?;

    stream
        .set_read_timeout(Some(timeout))
        .and_then(|_| stream.set_write_timeout(Some(timeout)))
        .map_err(|e| CoreError::internal(format!("could not configure the connection ({e})")))?;

    let request = match body {
        Some(body) => format!(
            "{method} {path} HTTP/1.1\r\n\
             Host: 127.0.0.1:{port}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n\
             {body}",
            body.len()
        ),
        None => format!(
            "{method} {path} HTTP/1.1\r\n\
             Host: 127.0.0.1:{port}\r\n\
             Connection: close\r\n\
             \r\n"
        ),
    };

    stream
        .write_all(request.as_bytes())
        .and_then(|_| stream.flush())
        .map_err(|e| CoreError::internal(format!("could not send to the local runtime ({e})")))?;

    read_response(stream)
}

/// Reads a status line, headers, and a body.
fn read_response(stream: TcpStream) -> CoreResult<String> {
    let mut reader = BufReader::new(stream);

    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|e| CoreError::internal(format!("no reply from the local runtime ({e})")))?;

    let status = parse_status(&status_line)?;

    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    let mut header_bytes = 0usize;

    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|e| CoreError::internal(format!("malformed reply headers ({e})")))?;

        if read == 0 {
            return Err(CoreError::internal("the local runtime closed mid-header"));
        }
        header_bytes += read;
        if header_bytes > MAX_HEADER_BYTES {
            return Err(CoreError::internal("reply headers are implausibly large"));
        }

        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break; // End of headers.
        }

        if let Some((name, value)) = trimmed.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim();

            match name.as_str() {
                "content-length" => {
                    let declared: usize = value.parse().map_err(|_| {
                        CoreError::internal("reply declared an unparsable content length")
                    })?;
                    if declared > MAX_RESPONSE_BYTES {
                        return Err(CoreError::internal("reply body exceeds the size limit"));
                    }
                    content_length = Some(declared);
                }
                "transfer-encoding" if value.eq_ignore_ascii_case("chunked") => {
                    chunked = true;
                }
                _ => {}
            }
        }
    }

    let body = if chunked {
        read_chunked(&mut reader)?
    } else {
        read_sized(&mut reader, content_length)?
    };

    if !(200..300).contains(&status) {
        // The body usually explains why; include a bounded excerpt.
        let excerpt: String = body.chars().take(200).collect();
        return Err(CoreError::internal(format!(
            "local runtime returned HTTP {status}: {excerpt}"
        )));
    }

    Ok(body)
}

fn parse_status(line: &str) -> CoreResult<u16> {
    // "HTTP/1.1 200 OK"
    line.split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| CoreError::internal("the local runtime sent a malformed status line"))
}

/// Reads a body of known length, or to EOF when the server did not declare one.
fn read_sized(reader: &mut BufReader<TcpStream>, length: Option<usize>) -> CoreResult<String> {
    let mut buffer = Vec::new();

    match length {
        Some(expected) => {
            buffer.resize(expected, 0);
            reader
                .read_exact(&mut buffer)
                .map_err(|e| CoreError::internal(format!("truncated reply body ({e})")))?;
        }
        None => {
            // `Connection: close` with no length: read to EOF, still bounded.
            reader
                .take(MAX_RESPONSE_BYTES as u64)
                .read_to_end(&mut buffer)
                .map_err(|e| CoreError::internal(format!("could not read the reply ({e})")))?;
        }
    }

    String::from_utf8(buffer)
        .map_err(|_| CoreError::internal("the local runtime sent a non-UTF-8 reply"))
}

/// Reads a chunked body, bounding the total.
fn read_chunked(reader: &mut BufReader<TcpStream>) -> CoreResult<String> {
    let mut body = Vec::new();

    loop {
        let mut size_line = String::new();
        reader
            .read_line(&mut size_line)
            .map_err(|e| CoreError::internal(format!("malformed chunk header ({e})")))?;

        // A chunk size may carry extensions after a ';'.
        let size_text = size_line.trim().split(';').next().unwrap_or("").trim();
        if size_text.is_empty() {
            return Err(CoreError::internal("empty chunk header"));
        }

        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| CoreError::internal("unparsable chunk size"))?;

        if size == 0 {
            break;
        }
        if body.len() + size > MAX_RESPONSE_BYTES {
            return Err(CoreError::internal("chunked reply exceeds the size limit"));
        }

        let mut chunk = vec![0u8; size];
        reader
            .read_exact(&mut chunk)
            .map_err(|e| CoreError::internal(format!("truncated chunk ({e})")))?;
        body.extend_from_slice(&chunk);

        // Trailing CRLF after each chunk.
        let mut terminator = [0u8; 2];
        reader
            .read_exact(&mut terminator)
            .map_err(|e| CoreError::internal(format!("malformed chunk terminator ({e})")))?;
    }

    String::from_utf8(body)
        .map_err(|_| CoreError::internal("the local runtime sent a non-UTF-8 reply"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_path_is_refused() {
        let err = post_json(1, "v1/chat", "{}", Duration::from_millis(50)).unwrap_err();
        assert!(err.message().contains("absolute"));

        let err = get(1, "health", Duration::from_millis(50)).unwrap_err();
        assert!(err.message().contains("absolute"));
    }

    #[test]
    fn a_get_carries_no_body_and_no_content_length() {
        // A GET with a Content-Length header makes some servers wait for a body
        // that never arrives. Verified by construction: the bodyless branch
        // emits neither header.
        let source = include_str!("loopback_http.rs");
        let implementation = source.split("#[cfg(test)]").next().unwrap();
        let none_branch = implementation
            .split("None => format!(")
            .nth(1)
            .and_then(|rest| rest.split("};").next())
            .expect("the bodyless request branch exists");

        assert!(!none_branch.contains("Content-Length"));
        assert!(!none_branch.contains("Content-Type"));
    }

    #[test]
    fn connecting_to_a_closed_local_port_fails_cleanly() {
        // Port 1 is not going to be serving anything; the point is that this
        // returns an error rather than hanging or panicking.
        let err = post_json(1, "/x", "{}", Duration::from_millis(200)).unwrap_err();
        assert_eq!(err.code(), "INTERNAL_ERROR");
        assert!(err.message().contains("unreachable"));
    }

    #[test]
    fn status_lines_are_parsed_and_malformed_ones_rejected() {
        assert_eq!(parse_status("HTTP/1.1 200 OK").unwrap(), 200);
        assert_eq!(
            parse_status("HTTP/1.1 503 Service Unavailable").unwrap(),
            503
        );

        for bad in ["", "garbage", "HTTP/1.1", "HTTP/1.1 not-a-number"] {
            assert!(parse_status(bad).is_err(), "should reject: {bad}");
        }
    }

    /// The security property this module exists for.
    ///
    /// Scans only the implementation, not this test module — otherwise the
    /// forbidden names written in the assertions below would match themselves.
    #[test]
    fn the_client_has_no_way_to_express_a_remote_host() {
        // `post_json` takes a port and nothing else; the address is built from
        // a hard-coded loopback constant. There is no hostname parameter, no
        // URL, and nothing that resolves a name, so a request cannot leave this
        // machine regardless of what a caller passes. (A DNS crate does exist
        // in the binary — `libp2p-mdns` parses DNS-format multicast packets —
        // but nothing on this path can reach it.)
        let full = include_str!("loopback_http.rs");
        let implementation = full
            .split("#[cfg(test)]")
            .next()
            .expect("the file has an implementation section");

        assert!(
            implementation.contains("Ipv4Addr::LOCALHOST"),
            "the destination must remain a hard-coded loopback constant"
        );

        // Introducing any of these would mean the client could resolve or reach
        // a host chosen at runtime.
        for forbidden in [
            "to_socket".to_string() + "_addrs",
            "lookup_host".to_string(),
        ] {
            assert!(
                !implementation.contains(&forbidden),
                "name resolution ({forbidden}) must not be introduced"
            );
        }
    }

    #[test]
    fn response_bounds_are_sane() {
        const { assert!(MAX_RESPONSE_BYTES >= 1024 * 1024) };
        const { assert!(MAX_HEADER_BYTES < MAX_RESPONSE_BYTES) };
    }
}
