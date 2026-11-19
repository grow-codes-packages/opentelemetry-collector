// Service integration tests (OpenTelemetry Collector): every public function against the real
// collector, with read-back through Jaeger and the capture files.
//! Round trips against `otel/opentelemetry-collector-contrib` (docker/compose.yml).

use std::time::Duration;

use grow_opentelemetry_collector as sdk;
use grow_opentelemetry_collector_integration as support;
use sdk::logging::{
    attrs, ExportOptions, LogClient, LogExporter, LogInput, LogQueryClient, LogRecord,
    SearchOptions, Severity,
};
use sdk::opentelemetry::metrics::MeterProvider as _;
use sdk::opentelemetry::trace::{Span as _, TraceContextExt as _, Tracer as _};
use sdk::tracing::{Protocol, TraceQueryClient, TracingConfig};
use sdk::{
    CollectorHealthClient, ErrorCode, InstrumentOptions, LogClientOptions, OtlpLogExporter, Signal,
    TelemetryOptions,
};
use serde_json::json;
use support::{load_bootstrap_env, run_id, var, wait_capture, wait_logs, wait_trace};

use sdk::OtelCollectorClient as Client;
use sdk::OtelCollectorConfig as Config;

fn env_config(
    f: impl FnOnce(sdk::OtelCollectorConfigBuilder) -> sdk::OtelCollectorConfigBuilder,
) -> Config {
    load_bootstrap_env();
    f(Config::builder()).build_from_env().expect("config")
}

#[tokio::test]
async fn client_against_collector() {
    let c = Client::new(env_config(|b| b)).unwrap();
    let hs = c.health().await.unwrap();
    assert!(hs.healthy, "{hs:?}");
    assert_eq!(hs.status, "ok");
    assert_eq!(hs.details["source"], "health_check");
    assert_eq!(hs.details["vendorStatus"], "Server available");
    assert!(!hs.details["upSince"].is_empty());
    assert!(
        c.wait_until_ready(Duration::from_secs(10))
            .await
            .unwrap()
            .healthy
    );
    assert!(c.collector_health().health().await.unwrap().healthy);
    assert!(c.ping().await.unwrap() > Duration::ZERO);
    let want = var("OTELCOL_TEST_SERVER_VERSION");
    assert_eq!(c.server_version().await.unwrap(), want);
    // target_info fallback: zpages pointed at a path without the page.
    let fb = Client::new(env_config(|b| {
        b.zpages_url(format!("{}/nope", var("OTELCOL_TEST_JAEGER_URL")))
    }))
    .unwrap();
    assert_eq!(fb.server_version().await.unwrap(), want);
    fb.close().await;
    // gRPC ping.
    let g = Client::new(env_config(|b| b.protocol(Protocol::Grpc))).unwrap();
    g.ping().await.unwrap();
    g.close().await;
    // Query clients: the collector has none.
    let id = var("OTELCOL_TEST_BOOTSTRAP_TRACE_ID");
    assert_eq!(
        c.trace_query().get_trace(&id).await.unwrap_err().code(),
        ErrorCode::Unsupported
    );
    assert_eq!(
        c.log_query()
            .search("x", SearchOptions::default())
            .await
            .unwrap_err()
            .code(),
        ErrorCode::Unsupported
    );
    assert!(!c.transport().is_closed());
    c.close().await;
    c.close().await;
    assert!(c.is_closed());
}

#[tokio::test]
async fn auth_and_errors() {
    load_bootstrap_env();
    let token = var("OTELCOL_TEST_BEARER_TOKEN");
    let auth_url = var("OTELCOL_TEST_AUTH_URL");
    let auth_grpc = var("OTELCOL_TEST_AUTH_GRPC_URL");
    let plain = |url: &str| Config::builder().url(url).build().unwrap();

    let no_auth = Client::new(plain(&auth_url)).unwrap();
    let err = no_auth.ping().await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Authentication, "{err}");
    assert!(err.message().contains("authorization"), "{err}");
    let wrong = Client::new(
        Config::builder()
            .url(&auth_url)
            .header("Authorization", format!("Bearer wrong-{token}"))
            .build()
            .unwrap(),
    )
    .unwrap();
    let err = wrong.ping().await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Authentication);
    assert!(!err.to_string().contains(&token));
    let with_auth_cfg = Config::builder()
        .url(&auth_url)
        .header("Authorization", format!("Bearer {token}"))
        .build()
        .unwrap();
    let with_auth = Client::new(with_auth_cfg.clone()).unwrap();
    with_auth.ping().await.unwrap();
    let grpc_no_auth = Client::new(
        Config::builder()
            .protocol(Protocol::Grpc)
            .grpc_url(&auth_grpc)
            .build()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        grpc_no_auth.ping().await.unwrap_err().code(),
        ErrorCode::Authentication
    );
    let grpc_auth = Client::new(
        Config::builder()
            .protocol(Protocol::Grpc)
            .grpc_url(&auth_grpc)
            .header("Authorization", format!("Bearer {token}"))
            .build()
            .unwrap(),
    )
    .unwrap();
    grpc_auth.ping().await.unwrap();

    // Log exporter: 401 without the token, 400 for a body above the 64 KiB limit.
    let exp = no_auth.log_exporter().unwrap();
    let err = exp
        .export(
            &[LogRecord::new(Severity::Info, "x")],
            ExportOptions::new(false),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Authentication, "{err}");
    let big = OtlpLogExporter::new(&with_auth_cfg).unwrap();
    let err = big
        .export(
            &[LogRecord::new(Severity::Info, "x".repeat(100_000))],
            ExportOptions::new(false),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Validation, "{err}");
    big.shutdown().await.unwrap();
    // Wrong base path → NOT_FOUND.
    let wrong_path = Client::new(env_config(|b| {
        b.signal_url(
            Signal::Traces,
            format!("{}/v1/nope", var("GROW_OPENTELEMETRY_COLLECTOR_URL")),
        )
    }))
    .unwrap();
    assert_eq!(
        wrong_path.ping().await.unwrap_err().code(),
        ErrorCode::NotFound
    );
    for c in [
        no_auth,
        wrong,
        with_auth,
        grpc_no_auth,
        grpc_auth,
        wrong_path,
    ] {
        c.close().await;
    }
}

#[tokio::test]
async fn stock_collector() {
    load_bootstrap_env();
    let stock_url = var("OTELCOL_TEST_STOCK_URL");
    let stock = Client::new(Config::builder().url(&stock_url).build().unwrap()).unwrap();
    // health_check binds localhost on stock stacks → zpages fallback.
    let hs = stock.health().await.unwrap();
    assert!(hs.healthy, "{hs:?}");
    assert_eq!(hs.details["source"], "zpages");
    assert_eq!(
        stock.server_version().await.unwrap(),
        var("OTELCOL_TEST_SERVER_VERSION")
    );
    stock.ping().await.unwrap();
    let h = CollectorHealthClient::new(
        &Config::builder()
            .url(&stock_url)
            .health_url(var("OTELCOL_TEST_STOCK_HEALTH_URL"))
            .build()
            .unwrap(),
    )
    .unwrap()
    .with_interval(Duration::from_millis(200));
    let hs = h.wait_until_ready(Duration::from_secs(5)).await.unwrap();
    assert_eq!(hs.details["source"], "zpages");
    h.close();
    let lc = stock.log_client(LogClientOptions::default()).unwrap();
    lc.send(LogInput::new(format!("stock {}", run_id())).into());
    lc.flush(Some(Duration::from_secs(10))).await.unwrap();
    assert_eq!(lc.stats().sent, 1);
    stock.close().await;
    let _ = CollectorHealthClient::from_env().map(|h| h.close());
}

#[tokio::test]
async fn log_exporter_and_log_client() {
    let run = run_id();
    for protocol in [Protocol::HttpProtobuf, Protocol::Grpc] {
        let cl = Client::new(env_config(|b| b.protocol(protocol))).unwrap();
        let exp = cl.log_exporter().unwrap();
        assert!(!exp.endpoint().is_empty());
        let marker = format!("exporter {} {run}", protocol.as_str());
        let mut r = LogRecord::new(Severity::Error, &marker)
            .with_attribute("grow.run", run.clone())
            .with_attribute("user.id", 42)
            .with_attribute("nested", json!({"k": "v"}))
            .with_trace("4bf92f3577b34da6a3ce929d0e0e4736", "00f067aa0ba902b7");
        r.trace_flags = Some(1);
        r.resource
            .insert("service.name".into(), json!(format!("rust-exporter-{run}")));
        let res = exp
            .export(std::slice::from_ref(&r), ExportOptions::new(true))
            .await
            .unwrap();
        assert_eq!((res.accepted, res.rejected), (1, 0));
        let got = wait_logs(&marker, 1).await.remove(0);
        assert_eq!(got.severity, Severity::Error);
        assert_eq!(
            got.trace_id.as_deref(),
            Some("4bf92f3577b34da6a3ce929d0e0e4736")
        );
        assert_eq!(got.span_id.as_deref(), Some("00f067aa0ba902b7"));
        assert_eq!(got.attributes["user.id"], json!(42));
        assert_eq!(got.attributes["nested"], json!({"k": "v"}));
        assert_eq!(
            got.resource["service.name"],
            json!(format!("rust-exporter-{run}"))
        );
        exp.shutdown().await.unwrap();
        cl.close().await;
    }

    // Client log client + Logger with redaction.
    let c = Client::new(env_config(|b| b.service_name(format!("rust-logs-{run}")))).unwrap();
    let lc = c.log_client(LogClientOptions::default()).unwrap();
    lc.logger(Some("checkout"), attrs! {"component" => "cart"})
        .warn(
            format!("logger {run}"),
            attrs! {"password" => format!("hunter2-{run}")},
        );
    lc.send_batch(vec![
        LogInput::new(format!("batch a {run}")).into(),
        LogInput::new(format!("batch b {run}")).into(),
    ]);
    lc.flush(Some(Duration::from_secs(10))).await.unwrap();
    let recs = wait_logs(&run, 5).await; // 2 exporter + logger + 2 batch
    let logger = recs
        .iter()
        .find(|r| r.body == format!("logger {run}"))
        .expect("logger record");
    assert_eq!(logger.attributes["password"], json!("[REDACTED]"));
    assert_eq!(logger.attributes["component"], json!("cart"));
    assert_eq!(
        logger.resource["service.name"],
        json!(format!("rust-logs-{run}"))
    );
    assert_eq!(lc.stats().sent, 3);
    assert_eq!(lc.stats().dropped, 0);
    // The read-back adapter used by the conformance suite.
    let found = support::CaptureLogQuery
        .search(&format!("batch a {run}"), SearchOptions::default())
        .await
        .unwrap();
    assert_eq!(found.records.len(), 1);
    c.close().await;
    // Package-level create_log_client.
    let plc = sdk::create_log_client(&env_config(|b| b), LogClientOptions::default()).unwrap();
    plc.send(LogInput::new(format!("package client {run}")).into());
    plc.close().await.unwrap();
    wait_logs(&format!("package client {run}"), 1).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_telemetry_round_trip() {
    for protocol in [Protocol::HttpProtobuf, Protocol::Grpc] {
        let run = run_id();
        let service = format!(
            "rust-telemetry-{}-{run}",
            protocol.as_str().replace('/', "-")
        );
        let cfg = env_config(|b| {
            b.protocol(protocol)
                .service_name(&service)
                .service_version("1.2.3")
                .environment("sdk-test")
                .resource_attribute("grow.run", run.clone())
                .runtime_metrics(true)
                .set_global(false)
                .metric_export_interval(Duration::from_secs(3600))
        });
        let h = if protocol == Protocol::Grpc {
            sdk::init_telemetry(&cfg).unwrap()
        } else {
            let c = Client::new(cfg.clone()).unwrap();
            let via_client = c.init_telemetry(TelemetryOptions::default()).unwrap();
            c.close().await;
            assert!(via_client.is_shutdown());
            sdk::init_telemetry_with(&cfg, TelemetryOptions::default()).unwrap()
        };
        // traces → Jaeger
        let tracer = h.tracer("grow-it", Some("1.0"));
        let root = tracer.start(format!("root-{run}"));
        let cx = sdk::opentelemetry::Context::current_with_span(root);
        let mut child = tracer.start_with_context("child", &cx);
        child.end();
        let trace_id = cx.span().span_context().trace_id().to_string();
        cx.span().end();
        // metrics → capture
        h.counter("grow_it_requests", InstrumentOptions::default().unit("1"))
            .unwrap()
            .add(3, &[]);
        h.histogram(
            "grow_it_latency",
            InstrumentOptions::default().boundaries(vec![10.0, 100.0]),
        )
        .unwrap()
        .record(50.0, &[]);
        h.gauge("grow_it_temperature", InstrumentOptions::default())
            .unwrap()
            .record(42.5, &[]);
        h.up_down_counter("grow_it_inflight", InstrumentOptions::default())
            .unwrap()
            .add(2, &[]);
        h.meter("custom")
            .u64_counter("grow_it_custom")
            .build()
            .add(1, &[]);
        // logs → capture (with trace correlation through the active span)
        h.logger(Some("it"), attrs! {})
            .info(format!("telemetry logclient {run}"), attrs! {});
        {
            let _guard = cx.clone().attach();
            h.log_client()
                .unwrap()
                .send(LogInput::new(format!("telemetry correlated {run}")).into());
        }
        h.force_flush(Some(Duration::from_secs(30))).await.unwrap();

        let tr = wait_trace(&trace_id, 2).await;
        assert_eq!(tr.spans[0].service_name, service);
        assert_eq!(tr.tree().len(), 1, "{tr:?}");
        let metrics = wait_capture(
            "metrics",
            &[
                &service,
                "grow_it_requests",
                "grow_it_latency",
                "grow_it_temperature",
                "grow_it_inflight",
                "grow_it_custom",
                "process.uptime",
            ],
        )
        .await;
        assert!(metrics.contains(r#""explicitBounds":[10,100]"#));
        let recs = wait_logs(&run, 2).await;
        let correlated = recs
            .iter()
            .find(|r| r.body == format!("telemetry correlated {run}"))
            .expect("correlated log");
        assert_eq!(correlated.trace_id.as_deref(), Some(trace_id.as_str()));
        assert_eq!(correlated.resource["service.name"], json!(service));
        assert_eq!(
            correlated.resource["deployment.environment.name"],
            json!("sdk-test")
        );
        assert!(correlated.resource.contains_key("host.name"));
        assert!(h.tracer_provider().is_some() && h.meter_provider().is_some());
        assert!(h.tracing().is_some() && h.runtime_metrics().is_some());
        assert_eq!(h.config().service_name(), service);
        assert!(!h.resource().is_empty());
        assert_eq!(h.resource_attributes()["grow.run"], json!(run));
        h.shutdown().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn global_metric_helpers_and_providers() {
    let run = run_id();
    let service = format!("rust-global-{run}");
    let cfg = env_config(|b| {
        b.service_name(&service)
            .set_global(true)
            .metric_export_interval(Duration::from_secs(3600))
    });
    let h = sdk::init_telemetry(&cfg).unwrap();
    sdk::counter("grow_global_counter", InstrumentOptions::default())
        .unwrap()
        .add(1, &[]);
    sdk::up_down_counter("grow_global_updown", InstrumentOptions::default())
        .unwrap()
        .add(1, &[]);
    sdk::histogram("grow_global_hist", InstrumentOptions::default())
        .unwrap()
        .record(1.0, &[]);
    sdk::gauge("grow_global_gauge", InstrumentOptions::default())
        .unwrap()
        .record(1.0, &[]);
    sdk::meter("")
        .u64_counter("grow_global_meter")
        .build()
        .add(1, &[]);
    h.force_flush(None).await.unwrap();
    wait_capture(
        "metrics",
        &[
            &service,
            "grow_global_counter",
            "grow_global_updown",
            "grow_global_hist",
            "grow_global_gauge",
            "grow_global_meter",
        ],
    )
    .await;
    h.shutdown().await.unwrap();

    // A meter provider built directly, with runtime metrics registered by hand.
    let run2 = run_id();
    let cfg = env_config(|b| b.service_name(format!("rust-provider-{run2}")));
    let mp = sdk::create_meter_provider(&cfg, sdk::detect_resource(&cfg), None).unwrap();
    let rt = sdk::register_runtime_metrics(&mp.meter("rt"));
    mp.meter("direct")
        .u64_counter("grow_direct_counter")
        .build()
        .add(5, &[]);
    let flush = mp.clone();
    tokio::task::spawn_blocking(move || flush.force_flush())
        .await
        .unwrap()
        .unwrap();
    wait_capture(
        "metrics",
        &[
            &format!("rust-provider-{run2}"),
            "grow_direct_counter",
            "process.cpu.time",
        ],
    )
    .await;
    rt.stop();
    tokio::task::spawn_blocking(move || mp.shutdown())
        .await
        .unwrap()
        .unwrap();
    let _ = sdk::metric_exporter(&cfg).unwrap();
    assert!(sdk::detect_resource_attributes(&sdk::ResourceDetector::ALL).contains_key("host.name"));
    assert_eq!(
        sdk::telemetry_resource_attributes(&cfg)["service.name"],
        json!(format!("rust-provider-{run2}"))
    );
    let _ = sdk::init_telemetry_from_env().map(|h| async move { h.shutdown().await });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_tracing_round_trip() {
    load_bootstrap_env();
    let run = run_id();
    // Package init_tracing from the environment, with the span helpers.
    let h =
        sdk::init_tracing(TracingConfig::new(format!("rust-tracing-{run}")).with_set_global(false))
            .unwrap();
    let tracer = h.tracer("it", None);
    let mut span = tracer.start(format!("traced-{run}"));
    span.add_event("checkpoint", vec![]);
    let trace_id = span.span_context().trace_id().to_string();
    span.end();
    h.force_flush(Some(Duration::from_secs(30))).await.unwrap();
    let tr = wait_trace(&trace_id, 1).await;
    assert_eq!(tr.spans[0].service_name, format!("rust-tracing-{run}"));
    assert_eq!(tr.spans[0].events[0].name, "checkpoint");
    h.shutdown().await.unwrap();

    // Client init_tracing over gRPC; tracing_config_from_env + collector_exporter.
    let c = Client::new(env_config(|b| b)).unwrap();
    let tc = sdk::tracing_config_from_env(
        TracingConfig::new(format!("rust-grpc-{run}")).with_protocol(Protocol::Grpc),
        &sdk::core::EnvReader::new(),
    )
    .unwrap();
    assert!(tc.endpoint.as_deref().unwrap().ends_with(":4317"), "{tc:?}");
    sdk::collector_exporter(&tc).unwrap();
    let gh = c
        .init_tracing(
            TracingConfig::new(format!("rust-grpc-{run}"))
                .with_protocol(Protocol::Grpc)
                .with_set_global(false),
        )
        .unwrap();
    let mut gspan = gh.tracer("it", None).start(format!("grpc-{run}"));
    let gid = gspan.span_context().span_id().to_string();
    let gtrace = gspan.span_context().trace_id().to_string();
    gspan.end();
    gh.force_flush(Some(Duration::from_secs(30))).await.unwrap();
    let tr = wait_trace(&gtrace, 1).await;
    assert_eq!(tr.spans[0].span_id, gid);
    assert_eq!(tr.spans[0].service_name, format!("rust-grpc-{run}"));
    assert!(tr.spans[0].attributes.is_empty() || !tr.spans[0].name.is_empty());
    c.close().await;
    assert!(gh.is_shutdown());

    // init_tracing_with_collector / tracing_config_from with an explicit collector config.
    let collector = env_config(|b| b.service_name(format!("rust-explicit-{run}")));
    let resolved = sdk::tracing_config_from(TracingConfig::new(""), &collector).unwrap();
    assert_eq!(resolved.service_name, format!("rust-explicit-{run}"));
    assert_eq!(
        resolved.endpoint.as_deref(),
        Some(
            sdk::resolve_traces_endpoint(Protocol::HttpProtobuf, resolved.endpoint.as_deref())
                .as_str()
        )
    );
    let eh =
        sdk::init_tracing_with_collector(TracingConfig::new("").with_set_global(false), &collector)
            .unwrap();
    let mut es = eh.tracer("it", None).start("explicit");
    let eid = es.span_context().trace_id().to_string();
    es.end();
    eh.force_flush(Some(Duration::from_secs(30))).await.unwrap();
    let tr = wait_trace(&eid, 1).await;
    assert_eq!(tr.spans[0].service_name, format!("rust-explicit-{run}"));
    eh.shutdown().await.unwrap();
    let wh = sdk::init_tracing_with_env(
        TracingConfig::new(format!("rust-env-{run}")).with_set_global(false),
        &sdk::core::EnvReader::new(),
    )
    .unwrap();
    wh.shutdown().await.unwrap();
    // The read-back adapter lists the services.
    let services = support::JaegerTraceQuery.list_services().await.unwrap();
    assert!(services.iter().any(|s| s == &format!("rust-tracing-{run}")) || !services.is_empty());
}
