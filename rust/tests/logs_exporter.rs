// Service tests (OpenTelemetry Collector): OTLP/HTTP log export on the official exporter.
//! OtlpLogExporter and create_log_client against a mocked OTLP/HTTP receiver.

mod common;
mod support;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use common::{closed_port, fast_retry};
use grow_opentelemetry_collector as sdk;
use opentelemetry_proto::tonic::common::v1::any_value::Value;
use sdk::logging::{
    attrs, BatchOptions, ExportOptions, LogClient, LogExporter, LogInput, LogRecord, Severity,
};
use sdk::{create_log_client, ErrorCode, LogClientOptions, OtlpLogExporter, Signal};
use serde_json::json;
use support::{attr, logs, requests, wait_requests};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use sdk::OtelCollectorConfig as Config;

fn config(url: &str) -> Config {
    Config::builder()
        .url(url)
        .service_name("logs-test")
        .header("Authorization", "Bearer log-secret-1")
        .retry(fast_retry(0.0))
        .build()
        .unwrap()
}

async fn receiver(status: u16, body: Vec<u8>) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/logs"))
        .respond_with(ResponseTemplate::new(status).set_body_raw(body, "application/x-protobuf"))
        .mount(&server)
        .await;
    server
}

fn record(body: &str) -> LogRecord {
    let mut r = LogRecord::new(Severity::Error, body)
        .with_attribute("user.id", 42)
        .with_attribute("ratio", 0.5)
        .with_attribute("ok", true)
        .with_attribute("nested", json!({"k": "v", "list": [1, "x", null]}))
        .with_attribute("dropped", serde_json::Value::Null)
        .with_trace("4bf92f3577b34da6a3ce929d0e0e4736", "00f067aa0ba902b7");
    r.trace_flags = Some(1);
    r.severity_text = "error".into();
    r.resource.insert("service.name".into(), json!("svc-a"));
    r
}

#[tokio::test]
async fn export_round_trip() {
    let server = receiver(200, vec![0x0a, 0x00]).await;
    let exp = OtlpLogExporter::new(&config(&server.uri())).unwrap();
    assert!(exp.endpoint().ends_with("/v1/logs"));
    assert!(format!("{exp:?}").contains("OtlpLogExporter"));
    let mut other = record("second");
    other.resource.insert("service.name".into(), json!("svc-b"));
    let mut plain = LogRecord::new(Severity::Info, "custom text");
    plain.severity_text = "notice".into();
    plain.resource = record("x").resource;
    plain.observed_timestamp = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(5));
    let res = exp
        .export(&[record("first"), other, plain], ExportOptions::new(true))
        .await
        .unwrap();
    assert_eq!((res.accepted, res.rejected), (3, 0));
    assert!(res.error.is_none());
    let reqs = requests(&server, "/v1/logs").await;
    assert_eq!(reqs.len(), 2, "one request per resource");
    assert_eq!(reqs[0].headers["content-encoding"], "gzip");
    assert_eq!(reqs[0].headers["authorization"], "Bearer log-secret-1");
    let first = logs(&reqs[0]);
    let rl = &first.resource_logs[0];
    assert_eq!(
        attr(&rl.resource.as_ref().unwrap().attributes, "service.name").as_deref(),
        Some("svc-a")
    );
    let sl = &rl.scope_logs[0];
    assert_eq!(sl.scope.as_ref().unwrap().name, sdk::INSTRUMENTATION_SCOPE);
    assert_eq!(sl.log_records.len(), 2);
    let r = &sl.log_records[0];
    assert_eq!(r.severity_number, 17);
    assert_eq!(r.severity_text, "ERROR");
    assert_eq!(hex(&r.trace_id), "4bf92f3577b34da6a3ce929d0e0e4736");
    assert_eq!(hex(&r.span_id), "00f067aa0ba902b7");
    assert_eq!(r.flags, 1);
    assert_eq!(attr(&r.attributes, "user.id").as_deref(), Some("42"));
    assert_eq!(attr(&r.attributes, "ok").as_deref(), Some("true"));
    assert_eq!(attr(&r.attributes, "ratio").as_deref(), Some("0.5"));
    assert!(attr(&r.attributes, "dropped").is_none());
    assert!(matches!(
        r.body.as_ref().unwrap().value,
        Some(Value::StringValue(ref s)) if s == "first"
    ));
    let nested = r
        .attributes
        .iter()
        .find(|kv| kv.key == "nested")
        .unwrap()
        .value
        .clone()
        .unwrap()
        .value
        .unwrap();
    assert!(matches!(nested, Value::KvlistValue(_)), "{nested:?}");
    let custom = &sl.log_records[1];
    assert_eq!(custom.severity_text, "INFO");
    assert_eq!(custom.observed_time_unix_nano, 5_000_000_000);
    assert!(custom.trace_id.is_empty());

    // Without gzip the body is plain protobuf.
    exp.export(&[record("plain")], ExportOptions::new(false))
        .await
        .unwrap();
    let reqs = requests(&server, "/v1/logs").await;
    assert!(reqs[2].headers.get("content-encoding").is_none());
    // Empty batches send nothing.
    let res = exp.export(&[], ExportOptions::default()).await.unwrap();
    assert_eq!(res.accepted, 0);
    assert_eq!(requests(&server, "/v1/logs").await.len(), 3);
    exp.shutdown().await.unwrap();
    exp.shutdown().await.unwrap();
    let err = exp
        .export(&[record("late")], ExportOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Cancelled);
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[tokio::test]
async fn partial_success_is_reported() {
    // partial_success { rejected_log_records: 2, error_message: "x" }
    let server = receiver(200, vec![0x0a, 0x05, 0x08, 0x02, 0x12, 0x01, b'x']).await;
    let exp = OtlpLogExporter::new(&config(&server.uri())).unwrap();
    let res = exp
        .export(
            &[record("a"), record("b"), record("c")],
            ExportOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!((res.accepted, res.rejected), (1, 2));
    let e = res.error.unwrap();
    assert_eq!(e.code(), ErrorCode::Validation);
    assert!(e.message().contains("2 of 3 records rejected: x"), "{e}");
    // A message without a count is still surfaced.
    let server = receiver(200, vec![0x0a, 0x03, 0x12, 0x01, b'w']).await;
    let exp = OtlpLogExporter::new(&config(&server.uri())).unwrap();
    let res = exp
        .export(&[record("a")], ExportOptions::default())
        .await
        .unwrap();
    assert_eq!((res.accepted, res.rejected), (1, 0));
    assert!(res.error.unwrap().message().contains("0 of 1"));
}

#[tokio::test]
async fn error_statuses_map_to_sdk_errors() {
    let status = |code: u8, msg: &str| {
        let mut b = vec![0x08, code, 0x12, msg.len() as u8];
        b.extend_from_slice(msg.as_bytes());
        b
    };
    let cases: Vec<(u16, Vec<u8>, ErrorCode, bool)> = vec![
        (
            401,
            status(
                16,
                "provided authorization does not match expected scheme or token",
            ),
            ErrorCode::Authentication,
            false,
        ),
        (
            400,
            status(3, "http: request body too large"),
            ErrorCode::Validation,
            false,
        ),
        (429, Vec::new(), ErrorCode::RateLimited, true),
        (503, status(14, "unavailable"), ErrorCode::Unavailable, true),
        (502, Vec::new(), ErrorCode::Unavailable, true),
        (
            404,
            b"404 page not found".to_vec(),
            ErrorCode::NotFound,
            false,
        ),
    ];
    for (code, body, want, retryable) in cases {
        let server = receiver(code, body).await;
        let exp = OtlpLogExporter::new(&config(&server.uri())).unwrap();
        let err = exp
            .export(&[record("x")], ExportOptions::default())
            .await
            .unwrap_err();
        assert_eq!(err.code(), want, "{code}: {err}");
        assert_eq!(err.http_status(), Some(code));
        assert_eq!(err.is_retryable(), retryable, "{code}");
        assert!(err.message().starts_with("logs.export: "), "{err}");
        assert!(!err.to_string().contains("log-secret-1"));
    }
    let server = MockServer::start().await;
    Mock::given(path("/v1/logs"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "3"))
        .mount(&server)
        .await;
    let err = OtlpLogExporter::new(&config(&server.uri()))
        .unwrap()
        .export(&[record("x")], ExportOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.retry_after(), Some(Duration::from_secs(3)));
}

#[tokio::test]
async fn transport_failures() {
    let dead = format!("http://127.0.0.1:{}", closed_port());
    let exp = OtlpLogExporter::new(&config(&dead)).unwrap();
    let err = exp
        .export(&[record("x")], ExportOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Unavailable);
    assert!(err.is_retryable());

    let server = MockServer::start().await;
    Mock::given(path("/v1/logs"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(2)))
        .mount(&server)
        .await;
    let cfg = Config::builder()
        .url(server.uri())
        .export_timeout(Duration::from_millis(100))
        .build()
        .unwrap();
    let err = OtlpLogExporter::new(&cfg)
        .unwrap()
        .export(&[record("x")], ExportOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Timeout);

    let bad = Config::builder()
        .url(server.uri())
        .ca_cert_path("/nonexistent/ca.pem")
        .build()
        .unwrap();
    assert_eq!(
        OtlpLogExporter::new(&bad).unwrap_err().code(),
        ErrorCode::Config
    );
    let err = Config::builder()
        .url(server.uri())
        .header("bad header", "x")
        .build()
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Config);
    let err = Config::builder()
        .signal_header(Signal::Logs, "x-ok", "bad\nvalue")
        .build()
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Config);
}

#[tokio::test]
async fn log_client_round_trip() {
    let server = receiver(200, vec![0x0a, 0x00]).await;
    let cfg = Config::builder()
        .url(server.uri())
        .service_name("checkout")
        .service_version("1.2.3")
        .environment("test")
        .resource_attribute("team", "core")
        .signal_url(Signal::Logs, format!("{}/v1/logs", server.uri()))
        .build()
        .unwrap();
    let lc = create_log_client(&cfg, LogClientOptions::default()).unwrap();
    lc.logger(Some("http"), attrs! {"component" => "cart"})
        .warn("hello", attrs! {"password" => "hunter2"});
    lc.send(LogInput::new("direct").into());
    lc.flush(Some(Duration::from_secs(5))).await.unwrap();
    let reqs = wait_requests(&server, "/v1/logs", 1).await;
    let req = logs(&reqs[0]);
    let res = &req.resource_logs[0].resource.as_ref().unwrap().attributes;
    assert_eq!(attr(res, "service.name").as_deref(), Some("checkout"));
    assert_eq!(attr(res, "service.version").as_deref(), Some("1.2.3"));
    assert_eq!(
        attr(res, "deployment.environment.name").as_deref(),
        Some("test")
    );
    assert_eq!(attr(res, "team").as_deref(), Some("core"));
    let recs = &req.resource_logs[0].scope_logs[0].log_records;
    let warn = recs.iter().find(|r| r.severity_number == 13).unwrap();
    assert_eq!(
        attr(&warn.attributes, "password").as_deref(),
        Some("[REDACTED]")
    );
    assert_eq!(
        attr(&warn.attributes, "logger.name").as_deref(),
        Some("http")
    );
    assert_eq!(lc.stats().sent, 2);
    lc.close().await.unwrap();

    // Options: own batch, resource, no redaction, min severity.
    let lc = create_log_client(
        &cfg,
        LogClientOptions {
            batch: Some(BatchOptions::default().gzip(false)),
            resource: Some(attrs! {"service.name" => "override"}),
            redaction: Some(None),
            min_severity: Some(Severity::Warn),
        },
    )
    .unwrap();
    let log = lc.logger(None, attrs! {});
    log.info("dropped by min severity", attrs! {});
    log.error("kept", attrs! {"password" => "visible"});
    lc.close().await.unwrap();
    let reqs = wait_requests(&server, "/v1/logs", 2).await;
    let req = logs(&reqs[1]);
    let rl = &req.resource_logs[0];
    assert_eq!(
        attr(&rl.resource.as_ref().unwrap().attributes, "service.name").as_deref(),
        Some("override")
    );
    let recs = &rl.scope_logs[0].log_records;
    assert_eq!(recs.len(), 1);
    assert_eq!(
        attr(&recs[0].attributes, "password").as_deref(),
        Some("visible")
    );
    assert!(reqs[1].headers.get("content-encoding").is_none());
}

#[tokio::test]
async fn log_client_retries_retryable_failures() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicU32::new(0));
    let c = calls.clone();
    Mock::given(path("/v1/logs"))
        .respond_with(move |_: &wiremock::Request| {
            if c.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(503)
            } else {
                ResponseTemplate::new(200)
            }
        })
        .mount(&server)
        .await;
    let lc = create_log_client(
        &config(&server.uri()),
        LogClientOptions {
            batch: Some(BatchOptions::default().retry_backoff(Duration::from_millis(10))),
            ..Default::default()
        },
    )
    .unwrap();
    lc.send(LogInput::new("retry me").into());
    lc.flush(Some(Duration::from_secs(5))).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(lc.stats().sent, 1);
    assert_eq!(lc.stats().retries, 1);
    lc.close().await.unwrap();
}
