// Service tests (OpenTelemetry Collector): init_telemetry, init_tracing and the metric helpers
// against a mocked OTLP/HTTP receiver.
//! Telemetry bootstrap over OTLP/HTTP.

mod common;
mod support;

use std::time::Duration;

use grow_opentelemetry_collector as sdk;
use opentelemetry::trace::{Span as _, Tracer as _};
use opentelemetry_proto::tonic::metrics::v1::metric::Data;
use sdk::core::EnvReader;
use sdk::logging::{attrs, LogClient, LogInput};
use sdk::tracing::{
    Compression, PropagatorKind, Protocol, SamplerKind, TraceBatchConfig, TracingConfig,
};
use sdk::{
    init_telemetry, init_telemetry_with, ErrorCode, InstrumentOptions, ResourceDetector, Signal,
    TelemetryOptions,
};
use serde_json::json;
use support::{attr, metrics, traces, wait_requests};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use sdk::OtelCollectorConfig as Config;

async fn receiver() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(vec![0x0a, 0x00], "application/x-protobuf"),
        )
        .mount(&server)
        .await;
    server
}

fn config(url: &str) -> sdk::OtelCollectorConfigBuilder {
    Config::builder()
        .url(url)
        .service_name("telemetry-test")
        .service_version("1.2.3")
        .environment("test")
        .resource_attribute("grow.run", "r1")
        .set_global(false)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_telemetry_round_trip() {
    let server = receiver().await;
    let cfg = config(&server.uri()).runtime_metrics(true).build().unwrap();
    let h = init_telemetry(&cfg).unwrap();
    assert!(format!("{h:?}").contains("telemetry-test"));
    assert_eq!(h.config().service_name(), "telemetry-test");
    assert_eq!(
        h.resource_attributes()["service.name"],
        json!("telemetry-test")
    );
    assert!(h.resource_attributes().contains_key("host.name"));
    assert!(h.resource_attributes().contains_key("process.pid"));
    assert!(!h.resource().is_empty());
    assert!(h.tracing().is_some() && h.tracer_provider().is_some());
    assert!(h.meter_provider().is_some() && h.log_client().is_some());
    assert!(h.runtime_metrics().is_some());

    let tracer = h.tracer("it", Some("0.1"));
    let mut root = tracer.start("root");
    let trace_id = root.span_context().trace_id().to_string();
    root.end();
    let mut unnamed = h.tracer("", None).start("unnamed");
    unnamed.end();
    h.counter(
        "it_requests",
        InstrumentOptions::default().unit("1").description("d"),
    )
    .unwrap()
    .add(3, &[]);
    h.up_down_counter(
        "it_inflight",
        InstrumentOptions::default().unit("1").description("d"),
    )
    .unwrap()
    .add(-1, &[]);
    h.histogram(
        "it_latency",
        InstrumentOptions::default()
            .boundaries(vec![10.0, 100.0])
            .unit("ms")
            .description("d"),
    )
    .unwrap()
    .record(50.0, &[]);
    h.gauge(
        "it_temperature",
        InstrumentOptions::default().unit("Cel").description("d"),
    )
    .unwrap()
    .record(42.5, &[]);
    use opentelemetry::metrics::MeterProvider as _;
    let _ = h.meter_provider().unwrap().meter("raw");
    h.meter("custom")
        .u64_counter("it_custom")
        .build()
        .add(1, &[]);
    h.logger(Some("it"), attrs! {"component" => "cart"})
        .info("telemetry log", attrs! {});
    h.log_client().unwrap().send(LogInput::new("direct").into());
    h.force_flush(Some(Duration::from_secs(10))).await.unwrap();

    let t = wait_requests(&server, "/v1/traces", 1).await;
    let spans: Vec<_> = t.iter().flat_map(|r| traces(r).resource_spans).collect();
    let rs = spans
        .iter()
        .find(|rs| {
            rs.scope_spans
                .iter()
                .flat_map(|s| &s.spans)
                .any(|s| s.name == "root")
        })
        .expect("root span");
    let res = &rs.resource.as_ref().unwrap().attributes;
    assert_eq!(attr(res, "service.name").as_deref(), Some("telemetry-test"));
    assert_eq!(attr(res, "service.version").as_deref(), Some("1.2.3"));
    assert_eq!(
        attr(res, "deployment.environment.name").as_deref(),
        Some("test")
    );
    assert_eq!(attr(res, "grow.run").as_deref(), Some("r1"));
    assert!(attr(res, "host.name").is_some());
    let root = rs
        .scope_spans
        .iter()
        .flat_map(|s| &s.spans)
        .find(|s| s.name == "root")
        .unwrap();
    assert_eq!(
        root.trace_id
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        trace_id
    );

    let m = wait_requests(&server, "/v1/metrics", 1).await;
    let names: Vec<(String, Option<Data>)> = m
        .iter()
        .flat_map(|r| metrics(r).resource_metrics)
        .flat_map(|rm| rm.scope_metrics)
        .flat_map(|sm| sm.metrics)
        .map(|m| (m.name, m.data))
        .collect();
    let has = |n: &str| names.iter().any(|(name, _)| name == n);
    for n in [
        "it_requests",
        "it_inflight",
        "it_latency",
        "it_temperature",
        "it_custom",
        "process.uptime",
    ] {
        assert!(has(n), "metric {n} missing: {names:?}");
    }
    let hist = names.iter().find(|(n, _)| n == "it_latency").unwrap();
    match &hist.1 {
        Some(Data::Histogram(h)) => assert_eq!(h.data_points[0].explicit_bounds, vec![10.0, 100.0]),
        other => panic!("{other:?}"),
    }
    match &names.iter().find(|(n, _)| n == "it_requests").unwrap().1 {
        Some(Data::Sum(s)) => {
            assert!(s.is_monotonic);
            assert_eq!(s.aggregation_temporality, 2);
        }
        other => panic!("{other:?}"),
    }
    let l = wait_requests(&server, "/v1/logs", 1).await;
    let bodies: Vec<String> = l
        .iter()
        .flat_map(|r| support::logs(r).resource_logs)
        .flat_map(|rl| rl.scope_logs)
        .flat_map(|sl| sl.log_records)
        .filter_map(|r| r.body.and_then(|b| b.value))
        .map(|v| format!("{v:?}"))
        .collect();
    assert!(
        bodies.iter().any(|b| b.contains("telemetry log")),
        "{bodies:?}"
    );
    assert!(bodies.iter().any(|b| b.contains("direct")));

    h.shutdown().await.unwrap();
    h.shutdown().await.unwrap();
    assert!(h.is_shutdown());
    assert!(h.runtime_metrics().unwrap().is_stopped());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signals_can_be_disabled() {
    let server = receiver().await;
    let cfg = config(&server.uri())
        .signal(Signal::Metrics, false)
        .resource_detectors(vec![])
        .build()
        .unwrap();
    let h = init_telemetry_with(
        &cfg,
        TelemetryOptions {
            traces: Some(false),
            logs: Some(false),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(h.tracing().is_none() && h.tracer_provider().is_none());
    assert!(h.meter_provider().is_none() && h.log_client().is_none());
    assert!(!h.resource_attributes().contains_key("host.name"));
    let mut span = h.tracer("noop", None).start("dropped");
    assert!(!span.is_recording());
    span.end();
    h.counter("dropped", InstrumentOptions::default())
        .unwrap()
        .add(1, &[]);
    let _ = h.meter("");
    h.logger(Some("x"), attrs! {}).error("dropped", attrs! {});
    h.logger(None, attrs! {}).error("dropped", attrs! {});
    let lc = h.logger(None, attrs! {});
    let _ = lc.child(attrs! {"a" => 1});
    h.force_flush(None).await.unwrap();
    h.shutdown().await.unwrap();
    assert!(server.received_requests().await.unwrap().is_empty());

    // Options can also turn signals on with a custom export interval and log batching.
    let h = init_telemetry_with(
        &cfg,
        TelemetryOptions {
            traces: Some(false),
            logs: Some(true),
            metrics: Some(true),
            set_global: Some(false),
            log_batch: Some(sdk::logging::BatchOptions::default().gzip(false)),
            min_severity: Some(sdk::logging::Severity::Error),
            metric_export_interval: Some(Duration::from_millis(100)),
            runtime_metrics: Some(false),
        },
    )
    .unwrap();
    h.gauge("g", InstrumentOptions::default())
        .unwrap()
        .record(1.0, &[]);
    assert!(!wait_requests(&server, "/v1/metrics", 1).await.is_empty());
    assert!(h.runtime_metrics().is_none());
    h.shutdown().await.unwrap();
}

#[tokio::test]
async fn init_telemetry_errors() {
    let bad = Config::builder()
        .url("http://127.0.0.1:1")
        .ca_cert_path("/nonexistent/ca.pem")
        .set_global(false)
        .build()
        .unwrap();
    // The trace exporter fails first.
    assert_eq!(init_telemetry(&bad).unwrap_err().code(), ErrorCode::Config);
    // Logs fail after traces started: traces are shut down again.
    let e = init_telemetry_with(
        &bad,
        TelemetryOptions {
            traces: Some(false),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Config);
    let e = init_telemetry_with(
        &bad,
        TelemetryOptions {
            traces: Some(false),
            logs: Some(false),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Config);
    let grpc_bad = Config::builder()
        .protocol(Protocol::Grpc)
        .insecure_skip_verify(true)
        .set_global(false)
        .build()
        .unwrap();
    let e = init_telemetry_with(
        &grpc_bad,
        TelemetryOptions {
            traces: Some(false),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Config);
    let e = init_telemetry_with(
        &Config::builder().set_global(false).build().unwrap(),
        TelemetryOptions {
            traces: Some(false),
            logs: Some(false),
            metric_export_interval: Some(Duration::ZERO),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Config);
}

#[test]
fn init_telemetry_needs_a_runtime() {
    let cfg = Config::builder().set_global(false).build().unwrap();
    assert_eq!(init_telemetry(&cfg).unwrap_err().code(), ErrorCode::Config);
    let e = sdk::create_meter_provider(&cfg, opentelemetry_sdk::Resource::builder().build(), None)
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Config);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn global_providers_and_helpers() {
    let server = receiver().await;
    let cfg = config(&server.uri())
        .set_global(true)
        .metric_export_interval(Duration::from_secs(3600))
        .build()
        .unwrap();
    let h = init_telemetry(&cfg).unwrap();
    sdk::counter("g_counter", InstrumentOptions::default().meter("m"))
        .unwrap()
        .add(1, &[]);
    sdk::up_down_counter("g_updown", InstrumentOptions::default())
        .unwrap()
        .add(1, &[]);
    sdk::histogram("g_hist", InstrumentOptions::default())
        .unwrap()
        .record(1.0, &[]);
    sdk::gauge("g_gauge", InstrumentOptions::default())
        .unwrap()
        .record(1.0, &[]);
    sdk::meter("").u64_counter("g_meter").build().add(1, &[]);
    let mut span = opentelemetry::global::tracer("global").start("global-span");
    span.end();
    h.force_flush(None).await.unwrap();
    let m = wait_requests(&server, "/v1/metrics", 1).await;
    let names: Vec<String> = m
        .iter()
        .flat_map(|r| metrics(r).resource_metrics)
        .flat_map(|rm| rm.scope_metrics)
        .flat_map(|sm| sm.metrics)
        .map(|m| m.name)
        .collect();
    for n in ["g_counter", "g_updown", "g_hist", "g_gauge", "g_meter"] {
        assert!(names.iter().any(|x| x == n), "{n}: {names:?}");
    }
    let t = wait_requests(&server, "/v1/traces", 1).await;
    let t = wait_requests(&server, "/v1/traces", t.len().max(1)).await;
    assert!(t
        .iter()
        .flat_map(|r| traces(r).resource_spans)
        .flat_map(|rs| rs.scope_spans)
        .flat_map(|ss| ss.spans)
        .any(|s| s.name == "global-span"));
    h.shutdown().await.unwrap();
    // Process environment entry point (it installs global providers too).
    if let Ok(h) = sdk::init_telemetry_from_env() {
        h.shutdown().await.unwrap();
    }
}

#[test]
fn instrument_validation() {
    let bad = |r: sdk::Result<()>| assert_eq!(r.unwrap_err().code(), ErrorCode::Validation);
    bad(sdk::counter("1bad", InstrumentOptions::default()).map(|_| ()));
    bad(sdk::up_down_counter("", InstrumentOptions::default()).map(|_| ()));
    bad(sdk::gauge("has space", InstrumentOptions::default()).map(|_| ()));
    bad(sdk::histogram(&"x".repeat(300), InstrumentOptions::default()).map(|_| ()));
    bad(sdk::histogram(
        "h",
        InstrumentOptions::default().boundaries(vec![10.0, 5.0]),
    )
    .map(|_| ()));
    bad(sdk::histogram("h", InstrumentOptions::default().boundaries(vec![f64::NAN])).map(|_| ()));
    assert!(sdk::counter("ok.name/x-y_z", InstrumentOptions::default()).is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_tracing_variants() {
    let server = receiver().await;
    let base = server.uri();
    let env = EnvReader::from_map([
        ("GROW_OPENTELEMETRY_COLLECTOR_URL".to_owned(), base.clone()),
        ("OTEL_SERVICE_NAME".to_owned(), "env-service".to_owned()),
        (
            "GROW_OTEL_RESOURCE_DETECTORS".to_owned(),
            "process".to_owned(),
        ),
    ]);
    let h = sdk::init_tracing_with_env(
        TracingConfig::new("")
            .with_set_global(false)
            .with_environment("dev"),
        &env,
    )
    .unwrap();
    assert_eq!(h.service_name(), "env-service");
    let mut s = h.tracer("it", None).start("env-span");
    s.end();
    h.force_flush(Some(Duration::from_secs(10))).await.unwrap();
    let t = wait_requests(&server, "/v1/traces", 1).await;
    let rs = &traces(&t[0]).resource_spans[0];
    let res = &rs.resource.as_ref().unwrap().attributes;
    assert_eq!(attr(res, "service.name").as_deref(), Some("env-service"));
    assert_eq!(
        attr(res, "deployment.environment.name").as_deref(),
        Some("dev")
    );
    assert!(attr(res, "process.pid").is_some());
    assert!(attr(res, "host.name").is_none());
    h.shutdown().await.unwrap();

    // Process environment entry point (whatever it contains, it must not panic).
    let _ = sdk::init_tracing(TracingConfig::new("x").with_set_global(false))
        .map(|h| tokio::spawn(async move { h.shutdown().await }));

    // Native is rejected everywhere.
    let native = TracingConfig::new("n").with_protocol(Protocol::Native);
    assert_eq!(
        sdk::init_tracing_with_env(native.clone(), &env)
            .unwrap_err()
            .code(),
        ErrorCode::Config
    );
    assert_eq!(
        sdk::tracing_config_from_env(native.clone(), &env)
            .unwrap_err()
            .code(),
        ErrorCode::Config
    );
    assert_eq!(
        sdk::collector_exporter(&native).unwrap_err().code(),
        ErrorCode::Config
    );
    let client =
        sdk::OtelCollectorClient::new(Config::builder().url(base).build().unwrap()).unwrap();
    assert_eq!(
        client.init_tracing(native).unwrap_err().code(),
        ErrorCode::Config
    );
    client.close().await;
}

#[test]
fn tracing_config_resolution() {
    let collector = Config::builder()
        .url("http://c:4318")
        .grpc_url("http://c:4317")
        .service_name("svc")
        .service_version("1")
        .environment("e")
        .header("x-common", "c")
        .resource_attribute("team", "core")
        .compression(Compression::None)
        .sampler(SamplerKind::AlwaysOff, 1.0)
        .propagators(vec![PropagatorKind::B3])
        .ca_cert_path("/x/ca.pem")
        .trace_batch(TraceBatchConfig {
            max_queue_size: 100,
            max_export_batch_size: 10,
            scheduled_delay: Duration::from_millis(50),
            export_timeout: Duration::from_secs(2),
        })
        .set_global(false)
        .build()
        .unwrap();
    let r = sdk::tracing_config_from(TracingConfig::new(""), &collector).unwrap();
    assert_eq!(r.service_name, "svc");
    assert_eq!(r.service_version.as_deref(), Some("1"));
    assert_eq!(r.environment.as_deref(), Some("e"));
    assert_eq!(r.endpoint.as_deref(), Some("http://c:4318/v1/traces"));
    assert_eq!(r.protocol, Protocol::HttpProtobuf);
    assert_eq!(r.headers["x-common"], "c");
    assert_eq!(r.resource_attributes["team"], json!("core"));
    assert_eq!(r.compression, Compression::None);
    assert_eq!(r.sampler, SamplerKind::AlwaysOff);
    assert_eq!(r.propagators, vec![PropagatorKind::B3]);
    assert!(r.tls.is_some());
    assert_eq!(r.batch.max_queue_size, 100);
    assert_eq!(r.batch.scheduled_delay, Duration::from_millis(50));
    assert!(!r.set_global);

    // Explicit values win; a protocol switch picks the other default endpoint.
    let explicit = TracingConfig::new("mine")
        .with_protocol(Protocol::Grpc)
        .with_header("x-common", "explicit")
        .with_resource_attribute("team", "mine")
        .with_sampler(SamplerKind::AlwaysOn, 1.0)
        .with_propagators(vec![PropagatorKind::TraceContext])
        .with_tls(sdk::core::TlsConfig::default())
        .with_batch(TraceBatchConfig {
            max_queue_size: 7,
            max_export_batch_size: 7,
            scheduled_delay: Duration::from_millis(7),
            export_timeout: Duration::from_millis(7),
        })
        .with_service_version("2")
        .with_environment("x");
    let r = sdk::tracing_config_from(explicit, &collector).unwrap();
    assert_eq!(r.service_name, "mine");
    assert_eq!(r.endpoint.as_deref(), Some("http://c:4317"));
    assert_eq!(r.headers["x-common"], "explicit");
    assert_eq!(r.resource_attributes["team"], json!("mine"));
    assert_eq!(r.sampler, SamplerKind::AlwaysOn);
    assert_eq!(r.propagators, vec![PropagatorKind::TraceContext]);
    assert_eq!(r.batch.max_queue_size, 7);
    assert_eq!(r.service_version.as_deref(), Some("2"));
    let grpc = Config::builder()
        .protocol(Protocol::Grpc)
        .grpc_url("http://g:4317")
        .build()
        .unwrap();
    let r = sdk::tracing_config_from(TracingConfig::new("s"), &grpc).unwrap();
    assert_eq!(r.protocol, Protocol::Grpc);
    assert_eq!(r.endpoint.as_deref(), Some("http://g:4317"));
    // An explicit endpoint without a path gets /v1/traces over HTTP.
    let r = sdk::tracing_config_from(
        TracingConfig::new("s").with_endpoint("http://x:4318"),
        &collector,
    )
    .unwrap();
    assert_eq!(r.endpoint.as_deref(), Some("http://x:4318/v1/traces"));
    let env = EnvReader::from_map([(
        "OTEL_EXPORTER_OTLP_ENDPOINT".to_owned(),
        "http://otel:4317".to_owned(),
    )]);
    let r =
        sdk::tracing_config_from_env(TracingConfig::new("s").with_protocol(Protocol::Grpc), &env)
            .unwrap();
    assert_eq!(r.endpoint.as_deref(), Some("http://otel:4317"));
    let r = sdk::tracing_config_from_env(TracingConfig::new("s"), &env).unwrap();
    assert_eq!(r.endpoint.as_deref(), Some("http://otel:4317/v1/traces"));
    assert_eq!(
        sdk::resolve_traces_endpoint(Protocol::HttpProtobuf, Some("http://h:1/")),
        "http://h:1/v1/traces"
    );
    assert_eq!(
        sdk::resolve_traces_endpoint(Protocol::HttpProtobuf, Some("http://h:1/x")),
        "http://h:1/x"
    );
    assert_eq!(
        sdk::resolve_traces_endpoint(Protocol::HttpProtobuf, None),
        sdk::DEFAULT_OTLP_HTTP_TRACES_ENDPOINT
    );
    assert_eq!(
        sdk::resolve_traces_endpoint(Protocol::Grpc, Some("")),
        sdk::DEFAULT_GRPC_URL
    );
    assert_eq!(
        sdk::resolve_traces_endpoint(Protocol::Grpc, Some("http://g:1")),
        "http://g:1"
    );
}

#[tokio::test]
async fn collector_exporter_settings() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    let tls = sdk::core::TlsConfig {
        ca_cert_path: Some(format!("{dir}/ca.pem").into()),
        client_cert_path: Some(format!("{dir}/client.pem").into()),
        client_key_path: Some(format!("{dir}/client.key").into()),
        insecure_skip_verify: true,
        ..Default::default()
    };
    let http = TracingConfig::new("s")
        .with_endpoint("https://localhost:4318")
        .with_tls(tls.clone())
        .with_compression(Compression::None);
    sdk::collector_exporter(&http).unwrap();
    let grpc = http.clone().with_protocol(Protocol::Grpc);
    assert_eq!(
        sdk::collector_exporter(&grpc).unwrap_err().code(),
        ErrorCode::Config
    );
    let grpc = TracingConfig::new("s")
        .with_protocol(Protocol::Grpc)
        .with_endpoint("https://localhost:4317")
        .with_header("x-a", "1")
        .with_tls(sdk::core::TlsConfig {
            insecure_skip_verify: false,
            ..tls
        });
    sdk::collector_exporter(&grpc).unwrap();
    let bad_header = TracingConfig::new("s")
        .with_protocol(Protocol::Grpc)
        .with_header("bad header", "x");
    assert_eq!(
        sdk::collector_exporter(&bad_header).unwrap_err().code(),
        ErrorCode::Config
    );
    let bad_ca = TracingConfig::new("s").with_tls(sdk::core::TlsConfig {
        ca_cert_path: Some("/nonexistent/ca.pem".into()),
        ..Default::default()
    });
    assert_eq!(
        sdk::collector_exporter(&bad_ca).unwrap_err().code(),
        ErrorCode::Config
    );
    let half = TracingConfig::new("s").with_tls(sdk::core::TlsConfig {
        client_cert_path: Some(format!("{dir}/client.pem").into()),
        ..Default::default()
    });
    assert_eq!(
        sdk::collector_exporter(&half).unwrap_err().code(),
        ErrorCode::Config
    );
    let pem = TracingConfig::new("s").with_tls(sdk::core::TlsConfig {
        ca_cert_pem: Some("not a pem".into()),
        ..Default::default()
    });
    assert_eq!(
        sdk::collector_exporter(&pem).unwrap_err().code(),
        ErrorCode::Config
    );
    // TLS on the metric exporter.
    let cfg = Config::builder()
        .ca_cert_path(format!("{dir}/ca.pem"))
        .proxy("http://proxy:3128")
        .compression(Compression::None)
        .build()
        .unwrap();
    sdk::metric_exporter(&cfg).unwrap();
    let cfg = Config::builder()
        .protocol(Protocol::Grpc)
        .grpc_url("https://localhost:4317")
        .compression(Compression::None)
        .build()
        .unwrap();
    sdk::metric_exporter(&cfg).unwrap();
    let detectors = ResourceDetector::ALL;
    assert_eq!(detectors.len(), 4);
}

#[tokio::test]
async fn runtime_metrics_handle() {
    use opentelemetry::metrics::MeterProvider as _;
    let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder().build();
    let h = sdk::register_runtime_metrics(&provider.meter("rt"));
    assert!(!h.is_stopped());
    h.stop();
    h.stop();
    assert!(h.is_stopped());
}
