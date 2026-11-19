// Service test helpers (OpenTelemetry Collector): OTLP request decoding for mocks.
#![allow(dead_code)]

use std::io::Read;

use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value::Value;
use opentelemetry_proto::tonic::common::v1::KeyValue;
use prost::Message;

/// The (gunzipped) body of a recorded request.
pub fn body(req: &wiremock::Request) -> Vec<u8> {
    let gz = req
        .headers
        .get("content-encoding")
        .is_some_and(|v| v.to_str().unwrap_or("") == "gzip");
    if gz {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(&req.body[..])
            .read_to_end(&mut out)
            .expect("gzip body");
        out
    } else {
        req.body.clone()
    }
}

pub fn logs(req: &wiremock::Request) -> ExportLogsServiceRequest {
    ExportLogsServiceRequest::decode(&body(req)[..]).expect("logs request")
}

pub fn traces(req: &wiremock::Request) -> ExportTraceServiceRequest {
    ExportTraceServiceRequest::decode(&body(req)[..]).expect("trace request")
}

pub fn metrics(req: &wiremock::Request) -> ExportMetricsServiceRequest {
    ExportMetricsServiceRequest::decode(&body(req)[..]).expect("metrics request")
}

/// A string-ish rendering of an attribute value.
pub fn attr(attrs: &[KeyValue], key: &str) -> Option<String> {
    attrs
        .iter()
        .find(|kv| kv.key == key)
        .and_then(|kv| kv.value.as_ref())
        .and_then(|v| v.value.as_ref())
        .map(|v| match v {
            Value::StringValue(s) => s.clone(),
            Value::IntValue(i) => i.to_string(),
            Value::BoolValue(b) => b.to_string(),
            Value::DoubleValue(d) => d.to_string(),
            other => format!("{other:?}"),
        })
}

/// Requests recorded by the server for a path.
pub async fn requests(server: &wiremock::MockServer, path: &str) -> Vec<wiremock::Request> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.url.path() == path)
        .collect()
}

/// Waits (up to 5 s) until the server recorded at least `n` requests for `path`.
pub async fn wait_requests(
    server: &wiremock::MockServer,
    path: &str,
    n: usize,
) -> Vec<wiremock::Request> {
    for _ in 0..100 {
        let r = requests(server, path).await;
        if r.len() >= n {
            return r;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    requests(server, path).await
}
