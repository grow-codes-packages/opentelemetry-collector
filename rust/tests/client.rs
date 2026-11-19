// Taken over from sdk-kit 0.1.0 (header removed): the skeleton tests ran against the demo
// service; these run the collector client (health_check + zpages, OTLP ping, version) against
// mocks of the collector's endpoints.
//! OtelCollectorClient against mocks of the collector (HTTP).

mod common;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::{closed_port, fast_retry};
use grow_opentelemetry_collector as sdk;
use sdk::core::ErrorCode;
use sdk::logging::{LogQueryClient, SearchOptions, TailOptions, WaitOptions};
use sdk::tracing::{DependencyOptions, TraceQueryClient, TraceSearchQuery, WaitForTraceOptions};
use sdk::{CollectorHealthClient, Signal};
use serde_json::json;
use wiremock::matchers::{body_bytes, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use sdk::OtelCollectorClient as Client;
use sdk::OtelCollectorConfig as Config;

const SERVICEZ: &str =
    r#"<table><tr><td><b>Version</b></td><td>&nbsp;</td><td class="v">v0.165.0</td></tr></table>"#;

fn builder(url: &str) -> sdk::OtelCollectorConfigBuilder {
    Config::builder()
        .url(url)
        .health_url(format!("{url}/"))
        .zpages_url(url)
        .retry(fast_retry(0.0))
}

fn client(url: &str) -> Client {
    Client::new(builder(url).build().unwrap()).unwrap()
}

fn rpc_status(code: u8, message: &str) -> Vec<u8> {
    let mut b = vec![0x08, code, 0x12, message.len() as u8];
    b.extend_from_slice(message.as_bytes());
    b
}

async fn collector() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "Server available",
            "upSince": "2026-11-18T10:00:00Z",
            "uptime": "1m0s",
            "ready": true,
            "n": 3,
            "nested": {"x": 1}
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/traces"))
        .and(header("content-type", "application/x-protobuf"))
        .and(header("authorization", "Bearer demo-token"))
        .and(body_bytes(Vec::<u8>::new()))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(vec![0x0a, 0x00], "application/x-protobuf"),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/traces"))
        .respond_with(ResponseTemplate::new(401).set_body_raw(
            rpc_status(16, "missing or empty authorization header: Authorization"),
            "application/x-protobuf",
        ))
        .mount(&server)
        .await;
    Mock::given(path("/debug/servicez"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(SERVICEZ, "text/html"))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn client_surface() {
    let server = collector().await;
    let c = Client::new(
        builder(&server.uri())
            .header("Authorization", "Bearer demo-token")
            .build()
            .unwrap(),
    )
    .unwrap();
    let h = c.health().await.unwrap();
    assert!(h.healthy);
    assert_eq!(h.status, "ok");
    assert_eq!(h.details["source"], "health_check");
    assert_eq!(h.details["vendorStatus"], "Server available");
    assert_eq!(h.details["upSince"], "2026-11-18T10:00:00Z");
    assert_eq!(h.details["ready"], "true");
    assert_eq!(h.details["n"], "3");
    assert!(!h.details.contains_key("nested"));
    assert!(
        c.wait_until_ready(Duration::from_secs(2))
            .await
            .unwrap()
            .healthy
    );
    assert!(c.collector_health().health().await.unwrap().healthy);
    assert!(c.ping().await.unwrap() < Duration::from_secs(5));
    assert_eq!(c.server_version().await.unwrap(), "0.165.0");
    assert!(!format!("{c:?}").contains("demo-token"));
    assert!(format!("{:?}", c.config()).contains("[REDACTED]"));

    // Missing token: 401 with a protobuf google.rpc.Status body.
    let none = client(&server.uri());
    let err = none.ping().await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Authentication);
    assert_eq!(err.http_status(), Some(401));
    assert!(err.message().contains("authorization header"), "{err}");
    assert!(!err.is_retryable());

    // A wrong token never appears in the error.
    let wrong = Client::new(
        builder(&server.uri())
            .header("Authorization", "Bearer wrong-token-123")
            .build()
            .unwrap(),
    )
    .unwrap();
    let err = wrong.ping().await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Authentication);
    assert!(!err.to_string().contains("wrong-token-123"));
}

#[tokio::test]
async fn ping_error_mapping() {
    let server = MockServer::start().await;
    Mock::given(path("/a/v1/traces"))
        .respond_with(ResponseTemplate::new(404).set_body_string("404 page not found"))
        .mount(&server)
        .await;
    Mock::given(path("/b/v1/traces"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(
                json!({"code": 3, "message": "ReadObject: expect { or n, but found x"}),
            ),
        )
        .mount(&server)
        .await;
    Mock::given(path("/c/v1/traces"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    Mock::given(path("/d/v1/traces"))
        .respond_with(
            ResponseTemplate::new(503)
                .insert_header("retry-after", "7")
                .set_body_raw(rpc_status(14, "memory limiter"), "application/x-protobuf"),
        )
        .mount(&server)
        .await;
    let retries = Arc::new(AtomicU32::new(0));
    let r = retries.clone();
    Mock::given(path("/e/v1/traces"))
        .respond_with(move |_: &wiremock::Request| {
            if r.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(429).insert_header("retry-after", "0")
            } else {
                ResponseTemplate::new(200)
            }
        })
        .mount(&server)
        .await;
    Mock::given(path("/f/v1/traces"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_raw(vec![0xff, 0xff, 0xff], "application/x-protobuf"),
        )
        .mount(&server)
        .await;
    Mock::given(path("/custom"))
        .and(header("x-trace-only", "1"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let base = server.uri();
    let ping = |p: &str| {
        let c = client(&format!("{base}/{p}"));
        async move { c.ping().await.unwrap_err() }
    };
    let e = ping("a").await;
    assert_eq!(e.code(), ErrorCode::NotFound);
    assert!(e.message().contains("404 page not found"), "{e}");
    let e = ping("b").await;
    assert_eq!(e.code(), ErrorCode::Validation);
    assert!(e.message().contains("ReadObject"), "{e}");
    assert_eq!(ping("c").await.code(), ErrorCode::PermissionDenied);
    let e = ping("d").await;
    assert_eq!(e.code(), ErrorCode::Unavailable);
    assert!(e.is_retryable());
    assert_eq!(e.retry_after(), Some(Duration::from_secs(7)));
    assert!(e.message().contains("memory limiter"));
    let e = ping("f").await;
    assert_eq!(e.code(), ErrorCode::Validation);
    assert!(e.message().ends_with("invalid request (HTTP 400)"), "{e}");
    client(&format!("{base}/e")).ping().await.unwrap();
    assert_eq!(retries.load(Ordering::SeqCst), 2);
    // A per-signal URL is used as is, with the per-signal headers.
    let c = Client::new(
        builder(&base)
            .signal_url(Signal::Traces, format!("{base}/custom"))
            .signal_header(Signal::Traces, "x-trace-only", "1")
            .build()
            .unwrap(),
    )
    .unwrap();
    c.ping().await.unwrap();
}

#[tokio::test]
async fn health_variants() {
    let server = MockServer::start().await;
    Mock::given(path("/unhealthy/"))
        .respond_with(
            ResponseTemplate::new(503).set_body_json(json!({"status": "Server not available"})),
        )
        .mount(&server)
        .await;
    Mock::given(path("/broken/"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    Mock::given(path("/zpages/debug/servicez"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(SERVICEZ, "text/html"))
        .mount(&server)
        .await;
    let base = server.uri();
    let health = |h: String, z: String| async move {
        Client::new(
            Config::builder()
                .url(base_url())
                .health_url(h)
                .zpages_url(z)
                .build()
                .unwrap(),
        )
        .unwrap()
        .health()
        .await
        .unwrap()
    };
    let dead = format!("http://127.0.0.1:{}/", closed_port());
    let u = health(format!("{base}/unhealthy/"), dead.clone()).await;
    assert_eq!((u.healthy, u.status.as_str()), (false, "unhealthy"));
    assert_eq!(u.details["httpStatus"], "503");
    assert_eq!(u.details["vendorStatus"], "Server not available");
    let b = health(format!("{base}/broken/"), dead.clone()).await;
    assert_eq!((b.healthy, b.status.as_str()), (false, "unhealthy"));
    assert_eq!(b.details["httpStatus"], "500");
    // health_check unreachable (stock stacks) → zpages fallback.
    let z = health(dead.clone(), format!("{base}/zpages")).await;
    assert_eq!((z.healthy, z.status.as_str()), (true, "ok"));
    assert_eq!(z.details["source"], "zpages");
    assert!(z.details.contains_key("healthCheckError"));
    // Both unreachable → unreachable.
    let n = health(dead.clone(), dead.trim_end_matches('/').to_owned()).await;
    assert_eq!((n.healthy, n.status.as_str()), (false, "unreachable"));
    // zpages answers with an error → the primary result stays.
    let e = health(dead.clone(), format!("{base}/nope")).await;
    assert_eq!(e.status, "unreachable");
}

fn base_url() -> String {
    format!("http://127.0.0.1:{}", closed_port())
}

#[tokio::test]
async fn wait_until_ready_polls_and_times_out() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicU32::new(0));
    let c2 = calls.clone();
    Mock::given(path("/"))
        .respond_with(move |_: &wiremock::Request| {
            if c2.fetch_add(1, Ordering::SeqCst) < 2 {
                ResponseTemplate::new(503)
            } else {
                ResponseTemplate::new(200).set_body_json(json!({"status": "Server available"}))
            }
        })
        .mount(&server)
        .await;
    let cfg = builder(&server.uri()).build().unwrap();
    let h = CollectorHealthClient::new(&cfg)
        .unwrap()
        .with_interval(Duration::from_millis(20));
    let hs = h.wait_until_ready(Duration::from_secs(5)).await.unwrap();
    assert!(hs.healthy);
    assert!(calls.load(Ordering::SeqCst) >= 3);

    let dead = Config::builder()
        .url(base_url())
        .zpages_url(base_url())
        .build()
        .unwrap();
    let h = CollectorHealthClient::new(&dead)
        .unwrap()
        .with_interval(Duration::from_millis(10));
    let err = h
        .wait_until_ready(Duration::from_millis(80))
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Timeout);
    assert!(err.message().contains("health.waitUntilReady"));
    // Zero means the default timeout; a closed standalone client fails with CANCELLED.
    h.close();
    assert_eq!(
        h.wait_until_ready(Duration::ZERO).await.unwrap_err().code(),
        ErrorCode::Cancelled
    );
    assert!(format!("{h:?}").contains("CollectorHealthClient"));
    let _ = CollectorHealthClient::from_env();
}

#[tokio::test]
async fn core_health_unreachable() {
    let url = base_url();
    let c = Client::new(
        Config::builder()
            .url(url.clone())
            .zpages_url(url)
            .build()
            .unwrap(),
    )
    .unwrap();
    let h = c.health().await.unwrap();
    assert!(!h.healthy);
    assert_eq!(h.status, "unreachable");
    assert!(h.details.contains_key("error"));
    // closed clients report CANCELLED, which health() returns as an error
    c.close().await;
    assert_eq!(c.health().await.unwrap_err().code(), ErrorCode::Cancelled);
    // a health client from a closed client is closed too; close() on it is a no-op
    let hc = c.collector_health();
    hc.close();
    assert_eq!(hc.health().await.unwrap_err().code(), ErrorCode::Cancelled);
}

#[tokio::test]
async fn server_version_fallbacks() {
    let server = MockServer::start().await;
    Mock::given(path("/plain/debug/servicez"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>no version</html>"))
        .mount(&server)
        .await;
    Mock::given(path("/denied/debug/servicez"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    Mock::given(path("/metrics"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "# HELP target_info x\ntarget_info{service_instance_id=\"a\",service_name=\"otelcol-contrib\",service_version=\"0.165.0\"} 1\n",
        ))
        .mount(&server)
        .await;
    Mock::given(path("/empty-metrics"))
        .respond_with(ResponseTemplate::new(200).set_body_string("up 1\n"))
        .mount(&server)
        .await;
    Mock::given(path("/denied-metrics"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let base = server.uri();
    let version = |z: &str, t: Option<&str>| {
        let mut b = Config::builder().url(base.clone()).zpages_url(z);
        if let Some(t) = t {
            b = b.telemetry_url(t);
        }
        let c = Client::new(b.build().unwrap()).unwrap();
        async move { c.server_version().await }
    };
    let metrics = format!("{base}/metrics");
    assert_eq!(
        version(&format!("{base}/plain"), Some(&metrics))
            .await
            .unwrap(),
        "0.165.0"
    );
    // zpages unreachable → telemetry fallback.
    assert_eq!(
        version(&base_url(), Some(&metrics)).await.unwrap(),
        "0.165.0"
    );
    let e = version(&format!("{base}/plain"), None).await.unwrap_err();
    assert_eq!(e.code(), ErrorCode::Unsupported);
    let e = version(
        &format!("{base}/plain"),
        Some(&format!("{base}/empty-metrics")),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Unsupported);
    let e = version(
        &format!("{base}/nope"),
        Some(&format!("{base}/nope-metrics")),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Unsupported);
    // Permission and authentication failures do not fall back.
    let e = version(&format!("{base}/denied"), Some(&metrics))
        .await
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::PermissionDenied);
    let e = version(
        &format!("{base}/plain"),
        Some(&format!("{base}/denied-metrics")),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Authentication);
    assert_eq!(
        sdk::parse_servicez_version(SERVICEZ).as_deref(),
        Some("0.165.0")
    );
    assert_eq!(sdk::parse_servicez_version("<b>Version</b>"), None);
    assert_eq!(
        sdk::parse_target_info_version("target_info{service_version=\"v1.2.3\"} 1").as_deref(),
        Some("1.2.3")
    );
}

#[tokio::test]
async fn query_clients_are_unsupported() {
    let c = client(&base_url());
    let tq = c.trace_query();
    let id = "4bf92f3577b34da6a3ce929d0e0e4736";
    let unsupported = |e: sdk::Error| {
        assert_eq!(e.code(), ErrorCode::Unsupported);
        assert!(e.message().contains("OpenTelemetry Collector"), "{e}");
    };
    unsupported(tq.get_trace(id).await.unwrap_err());
    unsupported(
        tq.search_traces(&TraceSearchQuery::default())
            .await
            .unwrap_err(),
    );
    unsupported(tq.list_services().await.unwrap_err());
    unsupported(tq.list_operations("svc").await.unwrap_err());
    unsupported(
        tq.get_dependencies(DependencyOptions::default())
            .await
            .unwrap_err(),
    );
    unsupported(
        tq.wait_for_trace(id, WaitForTraceOptions::default())
            .await
            .unwrap_err(),
    );
    let lq = c.log_query();
    unsupported(lq.search("x", SearchOptions::default()).await.unwrap_err());
    unsupported(
        lq.tail("x", Arc::new(|_| {}), TailOptions::default())
            .await
            .unwrap_err(),
    );
    unsupported(lq.list_streams().await.unwrap_err());
    unsupported(lq.list_labels().await.unwrap_err());
    unsupported(
        lq.wait_for_log("x", WaitOptions::default())
            .await
            .unwrap_err(),
    );
}

#[tokio::test]
async fn core_close_idempotent() {
    let server = collector().await;
    let c = client(&server.uri());
    let closed = Arc::new(AtomicU32::new(0));
    let counter = closed.clone();
    c.on_close(move || {
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
        })
    });
    let exporter = c.log_exporter().unwrap();
    let lc = c.log_client(Default::default()).unwrap();
    assert!(!c.is_closed());
    c.close().await;
    c.close().await;
    assert!(c.is_closed());
    assert_eq!(closed.load(Ordering::SeqCst), 1);
    assert_eq!(c.ping().await.unwrap_err().code(), ErrorCode::Cancelled);
    assert_eq!(
        c.server_version().await.unwrap_err().code(),
        ErrorCode::Cancelled
    );
    assert!(c.transport().is_closed());
    // The exporter and log client were shut down by close().
    use sdk::logging::{ExportOptions, LogClient, LogExporter, LogRecord, Severity};
    let err = exporter
        .export(
            &[LogRecord::new(Severity::Info, "x")],
            ExportOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Cancelled);
    assert!(lc.close().await.is_ok());
}

#[tokio::test]
async fn client_config_errors() {
    let err = Config::builder().url("mailto:x").build().unwrap_err();
    assert_eq!(err.code(), ErrorCode::Config);
    let cfg = Config::builder()
        .ca_cert_path("/nonexistent/ca.pem")
        .build()
        .unwrap();
    assert_eq!(
        Client::new(cfg.clone()).unwrap_err().code(),
        ErrorCode::Config
    );
    assert_eq!(
        CollectorHealthClient::new(&cfg).unwrap_err().code(),
        ErrorCode::Config
    );
    let _ = Client::from_env();
}
