// Created by sdk-kit 0.1.0; owned by this repository.
//! Helpers for the integration tests: bootstrap credentials, the conformance directory and the
//! test-only read-back adapters (DECISIONS.md §7). The collector stores nothing, so traces are
//! read back from the Jaeger sidecar (`OTELCOL_TEST_JAEGER_URL`, `/api/v3`) and logs and metrics
//! from the file exporter output served by the capture sidecar (`OTELCOL_TEST_CAPTURE_URL`).
//! None of this is shipped in the package.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use grow_opentelemetry_collector as sdk;
use sdk::logging::{
    AttrValue, Attributes, LogQueryClient, LogRecord, LogSearchResult, SearchOptions, Severity,
};
use sdk::tracing::{
    wait_for_trace, Trace, TraceQueryClient, TraceSearchQuery, TraceSummary, WaitForTraceOptions,
};
use sdk::{Error, Result};
use serde_json::{json, Value};

/// Loads `KEY=value` lines from `$GROW_BOOTSTRAP_ENV` (default `/bootstrap/env`) into the
/// process environment; variables that are already set win. A missing file is not an error.
pub fn load_bootstrap_env() {
    let path = std::env::var("GROW_BOOTSTRAP_ENV").unwrap_or_else(|_| "/bootstrap/env".to_owned());
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim().trim_start_matches("export ").trim();
            let value = value.trim().trim_matches('"');
            if !key.is_empty() && std::env::var_os(key).is_none() {
                std::env::set_var(key, value);
            }
        }
    }
}

/// `$GROW_CONFORMANCE_DIR`, else `<repo>/conformance/scenarios`.
pub fn conformance_dir() -> PathBuf {
    sdk::testing::scenarios_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
}

/// A required test variable from `/bootstrap/env`.
pub fn var(name: &str) -> String {
    load_bootstrap_env();
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{name} is not set (run through docker/compose.yml)"))
}

/// A unique run marker.
pub fn run_id() -> String {
    sdk::testing::run_id()
}

async fn get(url: &str) -> Result<(u16, String)> {
    let resp = reqwest::get(url)
        .await
        .map_err(|e| Error::unavailable(format!("readback: {e}")))?;
    let status = resp.status().as_u16();
    let text = resp
        .text()
        .await
        .map_err(|e| Error::unavailable(format!("readback: {e}")))?;
    Ok((status, text))
}

/// An OTLP JSON AnyValue as an attribute value (int64 strings become numbers).
pub fn any_value(v: &Value) -> AttrValue {
    if let Some(s) = v.get("stringValue") {
        return s.clone();
    }
    if let Some(b) = v.get("boolValue") {
        return b.clone();
    }
    if let Some(i) = v.get("intValue") {
        return match i {
            Value::String(s) => s.parse::<i64>().map(AttrValue::from).unwrap_or(Value::Null),
            other => other.clone(),
        };
    }
    if let Some(d) = v.get("doubleValue") {
        return d.clone();
    }
    if let Some(a) = v.get("arrayValue") {
        return Value::Array(
            a["values"]
                .as_array()
                .map(|vs| vs.iter().map(any_value).collect())
                .unwrap_or_default(),
        );
    }
    if let Some(kv) = v.get("kvlistValue") {
        return Value::Object(kv_map(&kv["values"]).into_iter().collect());
    }
    Value::Null
}

/// An OTLP JSON attribute list as a map.
pub fn kv_map(list: &Value) -> Attributes {
    list.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|kv| Some((kv["key"].as_str()?.to_owned(), any_value(&kv["value"]))))
                .collect()
        })
        .unwrap_or_default()
}

fn nanos(v: &Value) -> u64 {
    match v {
        Value::String(s) => s.parse().unwrap_or(0),
        Value::Number(n) => n.as_u64().unwrap_or(0),
        _ => 0,
    }
}

/// Every JSON line of one capture file (`traces`, `logs` or `metrics`).
pub async fn capture_lines(signal: &str) -> Result<Vec<Value>> {
    let (status, text) = get(&format!(
        "{}/{signal}.jsonl",
        var("OTELCOL_TEST_CAPTURE_URL")
    ))
    .await?;
    if status == 404 {
        return Ok(Vec::new());
    }
    if status != 200 {
        return Err(Error::unavailable(format!(
            "capture {signal}: HTTP {status}"
        )));
    }
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
}

/// The raw capture file text of a signal.
pub async fn capture_text(signal: &str) -> Result<String> {
    let (_, text) = get(&format!(
        "{}/{signal}.jsonl",
        var("OTELCOL_TEST_CAPTURE_URL")
    ))
    .await?;
    Ok(text)
}

/// Polls a capture file until it contains every needle (30 s).
pub async fn wait_capture(signal: &str, needles: &[&str]) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let text = capture_text(signal).await.unwrap_or_default();
        if needles.iter().all(|n| text.contains(n)) {
            return text;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "capture {signal} lacks {needles:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Every log record of `logs.jsonl`.
pub async fn capture_logs() -> Result<Vec<LogRecord>> {
    let mut out = Vec::new();
    for line in capture_lines("logs").await? {
        for rl in line["resourceLogs"].as_array().into_iter().flatten() {
            let resource = kv_map(&rl["resource"]["attributes"]);
            for sl in rl["scopeLogs"].as_array().into_iter().flatten() {
                for lr in sl["logRecords"].as_array().into_iter().flatten() {
                    let mut r = LogRecord::new(
                        Severity::from_number(lr["severityNumber"].as_u64().unwrap_or(0) as u8),
                        lr["body"]["stringValue"].as_str().unwrap_or_default(),
                    );
                    r.timestamp =
                        SystemTime::UNIX_EPOCH + Duration::from_nanos(nanos(&lr["timeUnixNano"]));
                    r.severity_text = lr["severityText"].as_str().unwrap_or_default().to_owned();
                    r.attributes = kv_map(&lr["attributes"]);
                    r.resource = resource.clone();
                    r.trace_id = lr["traceId"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned);
                    r.span_id = lr["spanId"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned);
                    out.push(r);
                }
            }
        }
    }
    Ok(out)
}

/// Polls the capture until at least `n` records contain `needle` in the body (30 s).
pub async fn wait_logs(needle: &str, n: usize) -> Vec<LogRecord> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let recs: Vec<LogRecord> = capture_logs()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.body.contains(needle))
            .collect();
        if recs.len() >= n {
            return recs;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "only {} logs contain {needle:?}",
            recs.len()
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Conformance [`LogQueryClient`]: substring match on the body of the captured records.
#[derive(Debug, Clone, Copy, Default)]
pub struct CaptureLogQuery;

#[async_trait]
impl LogQueryClient for CaptureLogQuery {
    async fn search(&self, query: &str, opts: SearchOptions) -> Result<LogSearchResult> {
        let mut records: Vec<LogRecord> = capture_logs()
            .await?
            .into_iter()
            .filter(|r| r.body.contains(query))
            .collect();
        records.truncate(opts.limit.max(1));
        Ok(LogSearchResult {
            records,
            next_cursor: None,
            total: None,
        })
    }
}

/// Conformance [`TraceQueryClient`] on the Jaeger v3 API.
#[derive(Debug, Clone, Copy, Default)]
pub struct JaegerTraceQuery;

const KINDS: [&str; 6] = [
    "unspecified",
    "internal",
    "server",
    "client",
    "producer",
    "consumer",
];
const STATUSES: [&str; 3] = ["unset", "ok", "error"];

#[async_trait]
impl TraceQueryClient for JaegerTraceQuery {
    async fn get_trace(&self, trace_id: &str) -> Result<Option<Trace>> {
        let (status, text) = get(&format!(
            "{}/api/v3/traces/{trace_id}",
            var("OTELCOL_TEST_JAEGER_URL")
        ))
        .await?;
        if status == 404 {
            return Ok(None);
        }
        if status != 200 {
            return Err(Error::unavailable(format!(
                "jaeger getTrace: HTTP {status}"
            )));
        }
        let doc: Value =
            serde_json::from_str(&text).map_err(|e| Error::unavailable(e.to_string()))?;
        let mut spans = Vec::new();
        for rs in doc["result"]["resourceSpans"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let res = kv_map(&rs["resource"]["attributes"]);
            let service = res
                .get("service.name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned();
            for ss in rs["scopeSpans"].as_array().into_iter().flatten() {
                for s in ss["spans"].as_array().into_iter().flatten() {
                    let start = nanos(&s["startTimeUnixNano"]) / 1000;
                    let end = nanos(&s["endTimeUnixNano"]) / 1000;
                    let kind = KINDS
                        .get(s["kind"].as_u64().unwrap_or(0) as usize)
                        .copied()
                        .unwrap_or("unspecified");
                    let code = STATUSES
                        .get(s["status"]["code"].as_u64().unwrap_or(0) as usize)
                        .copied()
                        .unwrap_or("unset");
                    let parent = s["parentSpanId"].as_str().filter(|p| !p.is_empty());
                    let events: Vec<Value> = s["events"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|e| json!({"name": e["name"], "timeMicros": nanos(&e["timeUnixNano"]) / 1000}))
                        .collect();
                    let span = json!({
                        "traceId": s["traceId"], "spanId": s["spanId"], "parentSpanId": parent,
                        "name": s["name"], "kind": kind, "serviceName": service,
                        "startMicros": start, "endMicros": end, "durationMicros": end.saturating_sub(start),
                        "status": {"code": code, "message": s["status"]["message"]},
                        "attributes": Value::Object(kv_map(&s["attributes"]).into_iter().collect()),
                        "events": events,
                    });
                    spans.push(
                        serde_json::from_value(span)
                            .map_err(|e| Error::unavailable(format!("span: {e}")))?,
                    );
                }
            }
        }
        Ok(Some(Trace::new(trace_id.to_owned(), spans)))
    }

    async fn search_traces(&self, _query: &TraceSearchQuery) -> Result<Vec<TraceSummary>> {
        Err(Error::unsupported("test adapter: searchTraces"))
    }

    async fn list_services(&self) -> Result<Vec<String>> {
        let (status, text) = get(&format!(
            "{}/api/v3/services",
            var("OTELCOL_TEST_JAEGER_URL")
        ))
        .await?;
        if status != 200 {
            return Err(Error::unavailable(format!(
                "jaeger services: HTTP {status}"
            )));
        }
        let doc: Value =
            serde_json::from_str(&text).map_err(|e| Error::unavailable(e.to_string()))?;
        let mut out: Vec<String> = doc["services"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_str().map(str::to_owned))
            .collect();
        out.sort();
        Ok(out)
    }

    async fn list_operations(&self, _service: &str) -> Result<Vec<String>> {
        Err(Error::unsupported("test adapter: listOperations"))
    }
}

/// Polls Jaeger until the trace has `min_spans` spans (30 s).
pub async fn wait_trace(trace_id: &str, min_spans: usize) -> Trace {
    wait_for_trace(
        &JaegerTraceQuery,
        trace_id,
        WaitForTraceOptions::default()
            .timeout(Duration::from_secs(30))
            .interval(Duration::from_millis(250))
            .min_spans(min_spans),
    )
    .await
    .unwrap_or_else(|e| panic!("trace {trace_id}: {e}"))
}
