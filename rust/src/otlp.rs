// Service code (OpenTelemetry Collector): OTLP wire helpers shared by ping and the exporters.
//! OTLP helpers: response decoding (partial success, `google.rpc.Status`), error mapping for
//! HTTP statuses and gRPC codes, TLS for the official exporters and a capturing HTTP client that
//! lets the SDK map an official exporter's response to its error types.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use bytes::Bytes;
use opentelemetry_http::{HttpClient, HttpError, Request, Response};
use opentelemetry_otlp::tonic_types::metadata::MetadataMap;
use opentelemetry_otlp::tonic_types::transport::{Certificate, ClientTlsConfig, Identity};

use crate::core::redact::{redact_text, truncate};
use crate::core::retry::parse_retry_after;
use crate::core::transport::{default_user_agent, MAX_VENDOR_MESSAGE};
use crate::core::{Error, ErrorCode, Result, TlsConfig};

/// A decoded OTLP `partial_success`: rejected items and the error message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PartialSuccess {
    /// Rejected spans, log records or data points.
    pub rejected: i64,
    /// The collector's message (may be empty).
    pub message: String,
}

/// Top-level varint and length-delimited fields of a protobuf message (first occurrence per
/// field number); `None` when the bytes are not a protobuf message.
type Fields<'a> = (BTreeMap<u64, u64>, BTreeMap<u64, &'a [u8]>);

fn read_varint(b: &[u8], pos: &mut usize) -> Option<u64> {
    let mut out = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*pos)?;
        *pos += 1;
        out |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(out);
        }
    }
    None
}

fn proto_fields(b: &[u8]) -> Option<Fields<'_>> {
    let mut varints = BTreeMap::new();
    let mut bytes = BTreeMap::new();
    let mut pos = 0;
    while pos < b.len() {
        let tag = read_varint(b, &mut pos)?;
        let (num, wire) = (tag >> 3, tag & 7);
        if num == 0 {
            return None;
        }
        match wire {
            0 => {
                let v = read_varint(b, &mut pos)?;
                varints.entry(num).or_insert(v);
            }
            1 => pos = pos.checked_add(8).filter(|p| *p <= b.len())?,
            2 => {
                let len = usize::try_from(read_varint(b, &mut pos)?).ok()?;
                let end = pos.checked_add(len).filter(|e| *e <= b.len())?;
                bytes.entry(num).or_insert(&b[pos..end]);
                pos = end;
            }
            5 => pos = pos.checked_add(4).filter(|p| *p <= b.len())?,
            _ => return None,
        }
    }
    Some((varints, bytes))
}

/// Decodes an `Export*ServiceResponse` body: field 1 `partial_success` {1: rejected count,
/// 2: error message}. An empty or undecodable body means full success.
pub fn decode_partial_success(body: &[u8]) -> PartialSuccess {
    let Some((_, fields)) = proto_fields(body) else {
        return PartialSuccess::default();
    };
    let Some(ps) = fields.get(&1) else {
        return PartialSuccess::default();
    };
    let Some((v, b)) = proto_fields(ps) else {
        return PartialSuccess::default();
    };
    PartialSuccess {
        rejected: v
            .get(&1)
            .map(|n| i64::try_from(*n).unwrap_or(i64::MAX))
            .unwrap_or(0),
        message: b
            .get(&2)
            .map(|m| String::from_utf8_lossy(m).into_owned())
            .unwrap_or_default(),
    }
}

/// Decodes a `google.rpc.Status` {1: code, 2: message}; `None` when it is not one.
pub fn decode_rpc_status(body: &[u8]) -> Option<(i32, String)> {
    let (v, b) = proto_fields(body)?;
    if v.is_empty() && b.is_empty() {
        return None;
    }
    let code = v
        .get(&1)
        .map(|c| i32::try_from(*c).unwrap_or(i32::MAX))
        .unwrap_or(0);
    let message = b
        .get(&2)
        .map(|m| String::from_utf8_lossy(m).into_owned())
        .unwrap_or_default();
    Some((code, message))
}

/// The error text of an OTLP/HTTP error response: a protobuf `google.rpc.Status`, a JSON
/// `{"code","message"}` or the first text line.
pub fn otlp_error_text(content_type: Option<&str>, body: &[u8]) -> Option<String> {
    if body.is_empty() {
        return None;
    }
    if content_type.is_some_and(|c| c.starts_with("application/x-protobuf")) {
        return decode_rpc_status(body)
            .map(|(_, m)| m)
            .filter(|m| !m.trim().is_empty());
    }
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) {
        return ["message", "error"]
            .iter()
            .find_map(|k| v.get(*k).and_then(|m| m.as_str()))
            .map(|m| m.trim().to_owned())
            .filter(|m| !m.is_empty());
    }
    let text = std::str::from_utf8(body).ok()?.trim();
    if text.is_empty() || text.starts_with('<') {
        return None;
    }
    text.lines().next().map(str::to_owned)
}

/// Maps an OTLP/HTTP error status to an SDK error: `operation: <reason>: <vendor text> (HTTP n)`
/// with the vendor text redacted and truncated; `Retry-After` is kept for 429 and 503.
pub fn http_status_error(
    operation: &str,
    status: u16,
    content_type: Option<&str>,
    retry_after: Option<&str>,
    body: &[u8],
    secrets: &[String],
) -> Error {
    let code = ErrorCode::from_http_status(status).unwrap_or(ErrorCode::Unavailable);
    let reason = match otlp_error_text(content_type, body) {
        Some(text) => format!(
            "{}: {}",
            code.reason(),
            truncate(&redact_text(&text, secrets), MAX_VENDOR_MESSAGE)
        ),
        None => code.reason().to_owned(),
    };
    let err = Error::from_status(status, format!("{operation}: {reason} (HTTP {status})"));
    let err = match status {
        429 | 502 | 503 | 504 => err.with_retryable(true),
        _ => err,
    };
    match (
        status,
        retry_after.and_then(|v| parse_retry_after(v, SystemTime::now())),
    ) {
        (429 | 503, Some(d)) => err.with_retry_after(d),
        _ => err,
    }
}

/// gRPC status codes by number.
const GRPC_CODES: [&str; 17] = [
    "OK",
    "CANCELLED",
    "UNKNOWN",
    "INVALID_ARGUMENT",
    "DEADLINE_EXCEEDED",
    "NOT_FOUND",
    "ALREADY_EXISTS",
    "PERMISSION_DENIED",
    "RESOURCE_EXHAUSTED",
    "FAILED_PRECONDITION",
    "ABORTED",
    "OUT_OF_RANGE",
    "UNIMPLEMENTED",
    "INTERNAL",
    "UNAVAILABLE",
    "DATA_LOSS",
    "UNAUTHENTICATED",
];

/// The SDK error code of a gRPC status code (OTLP rules: `INTERNAL` from a malformed message is
/// not retryable).
pub fn grpc_error_code(code: i32) -> ErrorCode {
    match code {
        16 => ErrorCode::Authentication,
        7 => ErrorCode::PermissionDenied,
        5 | 12 => ErrorCode::NotFound,
        6 => ErrorCode::Conflict,
        8 => ErrorCode::RateLimited,
        4 => ErrorCode::Timeout,
        1 => ErrorCode::Cancelled,
        3 | 9 | 11 | 13 => ErrorCode::Validation,
        _ => ErrorCode::Unavailable,
    }
}

/// Maps a gRPC status to an SDK error: `operation: <code name>: <message> (gRPC n)`.
pub fn grpc_status_error(operation: &str, code: i32, message: &str, secrets: &[String]) -> Error {
    let ec = grpc_error_code(code);
    let name = usize::try_from(code)
        .ok()
        .and_then(|c| GRPC_CODES.get(c))
        .map(|n| n.to_ascii_lowercase())
        .unwrap_or_else(|| "unknown".to_owned());
    let msg = redact_text(message.trim(), secrets);
    let reason = if msg.is_empty() {
        name
    } else {
        format!("{name}: {}", truncate(&msg, MAX_VENDOR_MESSAGE))
    };
    Error::new(ec, format!("{operation}: {reason} (gRPC {code})"))
}

/// Maps a tonic status to an SDK error.
pub(crate) fn tonic_error(operation: &str, status: &tonic::Status, secrets: &[String]) -> Error {
    grpc_status_error(operation, status.code() as i32, status.message(), secrets)
}

/// Parses the `Status { code: X, message: "..." }` debug text that the official tonic exporters
/// put into their errors.
pub(crate) fn parse_tonic_debug(text: &str) -> Option<(i32, String)> {
    let start = text.find("code: ")? + 6;
    let name: String = text[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    let code = match name.as_str() {
        "Ok" => 0,
        "Cancelled" => 1,
        "Unknown" => 2,
        "InvalidArgument" => 3,
        "DeadlineExceeded" => 4,
        "NotFound" => 5,
        "AlreadyExists" => 6,
        "PermissionDenied" => 7,
        "ResourceExhausted" => 8,
        "FailedPrecondition" => 9,
        "Aborted" => 10,
        "OutOfRange" => 11,
        "Unimplemented" => 12,
        "Internal" => 13,
        "Unavailable" => 14,
        "DataLoss" => 15,
        "Unauthenticated" => 16,
        _ => return None,
    };
    let message = text
        .find("message: \"")
        .map(|i| {
            let rest = &text[i + 10..];
            let mut out = String::new();
            let mut chars = rest.chars();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => {
                        if let Some(n) = chars.next() {
                            out.push(n);
                        }
                    }
                    '"' => break,
                    c => out.push(c),
                }
            }
            out
        })
        .unwrap_or_default();
    Some((code, message))
}

fn read_pem(path: &std::path::Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| {
        Error::config(format!("TLS file {} cannot be read", path.display())).with_source(e)
    })
}

fn ca_pem(tls: &TlsConfig) -> Result<Option<Vec<u8>>> {
    let mut pem = Vec::new();
    if let Some(path) = &tls.ca_cert_path {
        pem.extend(read_pem(path)?);
        pem.push(b'\n');
    }
    if let Some(text) = &tls.ca_cert_pem {
        pem.extend_from_slice(text.as_bytes());
    }
    Ok((!pem.is_empty()).then_some(pem))
}

fn identity_pem(tls: &TlsConfig) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
    match (&tls.client_cert_path, &tls.client_key_path) {
        (Some(cert), Some(key)) => Ok(Some((read_pem(cert)?, read_pem(key)?))),
        (None, None) => Ok(None),
        _ => Err(Error::config(
            "client certificate and client key must be given together",
        )),
    }
}

/// Settings of the HTTP client behind an official OTLP/HTTP exporter.
#[derive(Debug, Clone, Default)]
pub(crate) struct ClientSettings {
    pub tls: TlsConfig,
    pub proxy: Option<String>,
    pub connect_timeout: Option<Duration>,
    pub timeout: Duration,
}

/// A reqwest client with the TLS, proxy and timeout settings (environment proxies ignored).
pub(crate) fn reqwest_client(s: &ClientSettings) -> Result<reqwest::Client> {
    let mut b = reqwest::Client::builder()
        .use_rustls_tls()
        .no_proxy()
        .timeout(s.timeout)
        .user_agent(default_user_agent());
    if let Some(t) = s.connect_timeout {
        b = b.connect_timeout(t);
    }
    if let Some(ca) = ca_pem(&s.tls)? {
        let certs = reqwest::Certificate::from_pem_bundle(&ca)
            .map_err(|e| Error::config("the CA certificate is not valid PEM").with_source(e))?;
        if certs.is_empty() {
            return Err(Error::config("the CA certificate contains no certificate"));
        }
        for cert in certs {
            b = b.add_root_certificate(cert);
        }
    }
    if let Some((mut cert, key)) = identity_pem(&s.tls)? {
        cert.push(b'\n');
        cert.extend(key);
        let id = reqwest::Identity::from_pem(&cert).map_err(|e| {
            Error::config("the client certificate or key is not valid PEM").with_source(e)
        })?;
        b = b.identity(id);
    }
    if s.tls.insecure_skip_verify {
        b = b.danger_accept_invalid_certs(true);
    }
    if let Some(p) = &s.proxy {
        b = b.proxy(
            reqwest::Proxy::all(p)
                .map_err(|e| Error::config("proxy is not a valid URL").with_source(e))?,
        );
    }
    b.build()
        .map_err(|e| Error::config("cannot build the HTTP client").with_source(e))
}

/// The tonic TLS settings for an endpoint: `Some` for `https://` endpoints or when a CA or a
/// client certificate is configured. `insecure_skip_verify` is `CONFIG` (tonic cannot skip
/// verification; use `http/protobuf`).
pub(crate) fn tonic_tls(endpoint: &str, tls: &TlsConfig) -> Result<Option<ClientTlsConfig>> {
    if tls.insecure_skip_verify {
        return Err(Error::config(
            "insecureSkipVerify is not supported with OTLP gRPC in Rust; use http/protobuf",
        ));
    }
    let ca = ca_pem(tls)?;
    let identity = identity_pem(tls)?;
    if !endpoint.starts_with("https:") && ca.is_none() && identity.is_none() {
        return Ok(None);
    }
    let mut t = ClientTlsConfig::new().with_native_roots();
    if let Some(ca) = ca {
        t = t.ca_certificate(Certificate::from_pem(ca));
    }
    if let Some((cert, key)) = identity {
        t = t.identity(Identity::from_pem(cert, key));
    }
    if let Some(host) = url::Url::parse(endpoint)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
    {
        t = t.domain_name(host);
    }
    Ok(Some(t))
}

/// gRPC metadata from headers (names lower-cased; invalid names or values are `CONFIG`).
pub(crate) fn grpc_metadata(headers: &BTreeMap<String, String>) -> Result<MetadataMap> {
    let mut map = http::HeaderMap::new();
    for (k, v) in headers {
        let name = http::header::HeaderName::from_bytes(k.to_ascii_lowercase().as_bytes())
            .map_err(|_| Error::config(format!("invalid header name {k:?}")))?;
        let value = http::header::HeaderValue::from_str(v)
            .map_err(|_| Error::config(format!("invalid value for header {k:?}")))?;
        map.insert(name, value);
    }
    Ok(MetadataMap::from_headers(map))
}

/// What the capturing client saw for the last request.
#[derive(Debug, Clone)]
pub(crate) enum Captured {
    /// A response (any status).
    Response {
        status: u16,
        content_type: Option<String>,
        retry_after: Option<String>,
        body: Bytes,
    },
    /// A transport failure.
    Failed { timeout: bool },
}

/// Slot shared by a [`CaptureClient`] and its owner.
pub(crate) type CaptureSlot = Arc<Mutex<Option<Captured>>>;

/// [`HttpClient`] for the official OTLP/HTTP exporters that returns every response (also error
/// statuses) and records it so that the caller can map statuses, bodies and partial successes.
#[derive(Debug, Clone)]
pub(crate) struct CaptureClient {
    client: reqwest::Client,
    slot: CaptureSlot,
}

impl CaptureClient {
    pub(crate) fn new(client: reqwest::Client, slot: CaptureSlot) -> Self {
        CaptureClient { client, slot }
    }

    fn store(&self, c: Captured) {
        *self.slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(c);
    }
}

#[async_trait]
impl HttpClient for CaptureClient {
    async fn send_bytes(
        &self,
        request: Request<Bytes>,
    ) -> std::result::Result<Response<Bytes>, HttpError> {
        let request: reqwest::Request = request.try_into()?;
        let resp = match self.client.execute(request).await {
            Ok(r) => r,
            Err(e) => {
                self.store(Captured::Failed {
                    timeout: e.is_timeout(),
                });
                return Err(Box::new(e.without_url()));
            }
        };
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                self.store(Captured::Failed {
                    timeout: e.is_timeout(),
                });
                return Err(Box::new(e.without_url()));
            }
        };
        let header = |n: &str| {
            headers
                .get(n)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        self.store(Captured::Response {
            status: status.as_u16(),
            content_type: header("content-type"),
            retry_after: header("retry-after"),
            body: body.clone(),
        });
        let mut out = Response::builder().status(status);
        if let Some(h) = out.headers_mut() {
            *h = headers;
        }
        Ok(out.body(body)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protobuf_decoding() {
        assert_eq!(decode_partial_success(&[]), PartialSuccess::default());
        assert_eq!(
            decode_partial_success(&[0x0a, 0x00]),
            PartialSuccess::default()
        );
        let ps = decode_partial_success(&[0x0a, 0x05, 0x08, 0x02, 0x12, 0x01, b'x']);
        assert_eq!((ps.rejected, ps.message.as_str()), (2, "x"));
        // other fields (fixed64, fixed32) are skipped
        let body = [
            0x11, 1, 2, 3, 4, 5, 6, 7, 8, 0x1d, 1, 2, 3, 4, 0x0a, 0x02, 0x08, 0x05,
        ];
        assert_eq!(decode_partial_success(&body).rejected, 5);
        assert_eq!(decode_partial_success(&[0xff]), PartialSuccess::default());
        assert_eq!(
            decode_partial_success(&[0x08, 0x01]),
            PartialSuccess::default()
        );
        assert_eq!(
            decode_partial_success(&[0x0a, 0x02, 0x0b, 0x00]),
            PartialSuccess::default()
        );
        assert_eq!(
            decode_partial_success(&[0x0a, 0x09]),
            PartialSuccess::default()
        );
        assert_eq!(decode_partial_success(&[0x00]), PartialSuccess::default());
        assert_eq!(
            decode_partial_success(&[0x11, 0x00]),
            PartialSuccess::default()
        );
        assert_eq!(
            decode_partial_success(&[0x0d, 0x00]),
            PartialSuccess::default()
        );
        assert_eq!(
            decode_partial_success(&[
                0x08, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01
            ]),
            PartialSuccess::default()
        );
        let huge = [
            0x0a, 0x0b, 0x08, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01,
        ];
        assert_eq!(decode_partial_success(&huge).rejected, i64::MAX);
        assert_eq!(
            decode_rpc_status(&[0x08, 0x10, 0x12, 0x02, b'n', b'o']),
            Some((16, "no".to_owned()))
        );
        assert_eq!(
            decode_rpc_status(&[0x12, 0x01, b'm']),
            Some((0, "m".into()))
        );
        assert_eq!(decode_rpc_status(&[0x08, 0x03]), Some((3, String::new())));
        assert_eq!(decode_rpc_status(&[]), None);
        assert_eq!(decode_rpc_status(&[0x07]), None);
    }

    #[test]
    fn error_texts() {
        let pb = Some("application/x-protobuf");
        assert_eq!(
            otlp_error_text(pb, &[0x08, 0x03, 0x12, 0x01, b'z']).as_deref(),
            Some("z")
        );
        assert_eq!(otlp_error_text(pb, &[0x08, 0x03]), None);
        assert_eq!(otlp_error_text(pb, &[]), None);
        let json = br#"{"code":3,"message":" bad "}"#;
        assert_eq!(otlp_error_text(None, json).as_deref(), Some("bad"));
        assert_eq!(
            otlp_error_text(None, br#"{"error":"e"}"#).as_deref(),
            Some("e")
        );
        assert_eq!(otlp_error_text(None, br#"{"x":1}"#), None);
        assert_eq!(otlp_error_text(None, b"<html>").as_deref(), None);
        assert_eq!(otlp_error_text(None, b"  ").as_deref(), None);
        assert_eq!(otlp_error_text(None, &[0xff, 0xfe]), None);
        assert_eq!(
            otlp_error_text(Some("text/plain"), b"line1\nline2").as_deref(),
            Some("line1")
        );
        let e = http_status_error(
            "op",
            503,
            None,
            Some("2"),
            b"down secret-1",
            &["secret-1".into()],
        );
        assert_eq!(e.code(), ErrorCode::Unavailable);
        assert_eq!(e.retry_after(), Some(Duration::from_secs(2)));
        assert!(e.message().contains("[REDACTED]"), "{e}");
        let e = http_status_error("op", 400, None, Some("2"), b"", &[]);
        assert_eq!(e.retry_after(), None);
        assert_eq!(e.message(), "op: invalid request (HTTP 400)");
        assert_eq!(
            http_status_error("op", 302, None, None, b"", &[]).code(),
            ErrorCode::Unavailable
        );
    }

    #[test]
    fn grpc_codes() {
        let cases = [
            (0, ErrorCode::Unavailable),
            (1, ErrorCode::Cancelled),
            (2, ErrorCode::Unavailable),
            (3, ErrorCode::Validation),
            (4, ErrorCode::Timeout),
            (5, ErrorCode::NotFound),
            (6, ErrorCode::Conflict),
            (7, ErrorCode::PermissionDenied),
            (8, ErrorCode::RateLimited),
            (9, ErrorCode::Validation),
            (10, ErrorCode::Unavailable),
            (11, ErrorCode::Validation),
            (12, ErrorCode::NotFound),
            (13, ErrorCode::Validation),
            (14, ErrorCode::Unavailable),
            (15, ErrorCode::Unavailable),
            (16, ErrorCode::Authentication),
            (99, ErrorCode::Unavailable),
        ];
        for (code, want) in cases {
            assert_eq!(grpc_error_code(code), want, "{code}");
        }
        let e = grpc_status_error("ping", 16, " tok-1 bad ", &["tok-1".into()]);
        assert_eq!(
            e.message(),
            "ping: unauthenticated: [REDACTED] bad (gRPC 16)"
        );
        assert_eq!(
            grpc_status_error("ping", 99, "", &[]).message(),
            "ping: unknown (gRPC 99)"
        );
        assert_eq!(
            grpc_status_error("ping", 14, "", &[]).message(),
            "ping: unavailable (gRPC 14)"
        );
    }

    #[test]
    fn tonic_debug_text() {
        let names = [
            "Ok",
            "Cancelled",
            "Unknown",
            "InvalidArgument",
            "DeadlineExceeded",
            "NotFound",
            "AlreadyExists",
            "PermissionDenied",
            "ResourceExhausted",
            "FailedPrecondition",
            "Aborted",
            "OutOfRange",
            "Unimplemented",
            "Internal",
            "Unavailable",
            "DataLoss",
            "Unauthenticated",
        ];
        for (i, n) in names.iter().enumerate() {
            let text = format!(
                "export error: Status {{ code: {n}, message: \"m \\\"q\\\"\", details: [] }}"
            );
            assert_eq!(
                parse_tonic_debug(&text),
                Some((i as i32, "m \"q\"".to_owned()))
            );
        }
        assert_eq!(parse_tonic_debug("code: Weird"), None);
        assert_eq!(parse_tonic_debug("no status"), None);
        assert_eq!(
            parse_tonic_debug("code: Internal"),
            Some((13, String::new()))
        );
        assert_eq!(
            parse_tonic_debug("code: Internal, message: \"trailing\\"),
            Some((13, "trailing".to_owned()))
        );
    }

    #[test]
    fn tls_helpers() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
        assert!(tonic_tls("http://h:4317", &TlsConfig::default())
            .unwrap()
            .is_none());
        assert!(tonic_tls("https://h:4317", &TlsConfig::default())
            .unwrap()
            .is_some());
        let pem = std::fs::read_to_string(format!("{dir}/ca.pem")).unwrap();
        let t = TlsConfig {
            ca_cert_pem: Some(pem.clone()),
            ..Default::default()
        };
        assert!(tonic_tls("http://h:4317", &t).unwrap().is_some());
        assert!(tonic_tls("not a url", &t).unwrap().is_some());
        let s = ClientSettings {
            tls: t,
            proxy: Some("::bad::".into()),
            connect_timeout: None,
            timeout: Duration::from_secs(1),
        };
        assert_eq!(reqwest_client(&s).unwrap_err().code(), ErrorCode::Config);
        let empty = ClientSettings {
            tls: TlsConfig {
                ca_cert_pem: Some("-----BEGIN NOTHING-----".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            reqwest_client(&empty).unwrap_err().code(),
            ErrorCode::Config
        );
        let bad_id = ClientSettings {
            tls: TlsConfig {
                client_cert_path: Some(format!("{dir}/ca.pem").into()),
                client_key_path: Some(format!("{dir}/ca.pem").into()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            reqwest_client(&bad_id).unwrap_err().code(),
            ErrorCode::Config
        );
        let mut bad = BTreeMap::new();
        bad.insert("x".to_owned(), "bad\nvalue".to_owned());
        assert_eq!(grpc_metadata(&bad).unwrap_err().code(), ErrorCode::Config);
    }
}
