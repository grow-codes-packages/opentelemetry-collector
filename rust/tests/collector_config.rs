// Service tests (OpenTelemetry Collector): configuration, environment precedence, validation.
//! OtelCollectorConfig: defaults, `GROW_OPENTELEMETRY_COLLECTOR_*` > `GROW_OTEL_*` > `OTEL_*`.

use std::path::PathBuf;
use std::time::Duration;

use grow_opentelemetry_collector as sdk;
use sdk::core::EnvReader;
use sdk::logging::BatchOptions;
use sdk::tracing::{Compression, PropagatorKind, Protocol, SamplerKind, TraceBatchConfig};
use sdk::{ErrorCode, ResourceDetector, Signal};
use serde_json::json;

use sdk::OtelCollectorConfig as Config;

fn env(vars: &[(&str, &str)]) -> EnvReader {
    EnvReader::from_map(vars.iter().map(|(k, v)| (k.to_string(), v.to_string())))
}

fn from(vars: &[(&str, &str)]) -> Config {
    Config::builder().build_with_env(&env(vars)).unwrap()
}

fn err(vars: &[(&str, &str)]) -> sdk::Error {
    Config::builder().build_with_env(&env(vars)).unwrap_err()
}

#[test]
fn defaults() {
    let c = Config::builder().build().unwrap();
    assert_eq!(c.url().as_str(), "http://127.0.0.1:4318/");
    assert_eq!(c.grpc_url().as_str(), "http://127.0.0.1:4317/");
    let derived = Config::builder()
        .url("https://col.internal:4318/base")
        .build()
        .unwrap();
    assert_eq!(derived.grpc_url().as_str(), "https://col.internal:4317/");
    assert_eq!(c.protocol(), Protocol::HttpProtobuf);
    assert_eq!(c.health_url().as_str(), "http://127.0.0.1:13133/");
    assert_eq!(c.zpages_url().as_str(), "http://127.0.0.1:55679/");
    assert!(c.telemetry_url().is_none());
    assert_eq!(c.compression(), Compression::Gzip);
    assert_eq!(c.export_timeout(), Duration::from_secs(10));
    assert!(c.service_name().starts_with("unknown_service:"));
    assert_eq!(c.service_version(), None);
    assert_eq!(c.environment(), None);
    assert_eq!(c.resource_detectors(), ResourceDetector::ALL);
    assert!(Signal::ALL.iter().all(|s| c.signal_enabled(*s)));
    assert_eq!(c.sampler(), SamplerKind::ParentBasedAlwaysOn);
    assert_eq!(c.sampler_ratio(), 1.0);
    assert_eq!(
        c.propagators(),
        [PropagatorKind::TraceContext, PropagatorKind::Baggage]
    );
    assert_eq!(c.trace_batch().export_timeout, Duration::from_secs(10));
    assert_eq!(c.log_batch().max_queue_size, 2048);
    assert_eq!(c.metric_export_interval(), Duration::from_secs(60));
    assert!(!c.runtime_metrics());
    assert!(c.set_global());
    assert_eq!(
        c.signal_endpoint(Signal::Logs),
        "http://127.0.0.1:4318/v1/logs"
    );
    assert!(sdk::default_service_name().starts_with("unknown_service:"));
    assert_eq!(Signal::Metrics.as_str(), "metrics");
    // Admin endpoints follow the url host and scheme, without user info.
    let c = Config::builder()
        .url("https://u:p@collector.example:4318/base?x=1")
        .build()
        .unwrap();
    assert_eq!(c.health_url().as_str(), "https://collector.example:13133/");
    assert_eq!(c.zpages_url().as_str(), "https://collector.example:55679/");
}

#[test]
fn explicit_values() {
    let c = Config::builder()
        .url("http://c:4318/")
        .grpc_url("http://c:4317")
        .protocol(Protocol::Grpc)
        .signal_url(Signal::Metrics, "http://m:4317")
        .signal_url(Signal::Logs, "http://l:4318/custom")
        .signal_url(Signal::Traces, "http://t:4318/v1/traces")
        .signal_header(Signal::Traces, "x-t", "1")
        .header("x-common", "c")
        .header("x-t", "common")
        .compression(Compression::None)
        .export_timeout(Duration::from_secs(3))
        .health_url("http://h:1/")
        .zpages_url("http://z:2")
        .telemetry_url("http://c:8888/metrics")
        .service_name("svc")
        .service_version("1.0")
        .environment("prod")
        .resource_attribute("team", "core")
        .resource_detectors(vec![ResourceDetector::Host])
        .signal(Signal::Logs, false)
        .signal(Signal::Traces, true)
        .signal(Signal::Metrics, false)
        .sampler(SamplerKind::TraceIdRatio, 0.25)
        .propagators(vec![PropagatorKind::B3])
        .trace_batch(TraceBatchConfig {
            max_queue_size: 10,
            max_export_batch_size: 5,
            scheduled_delay: Duration::from_millis(100),
            export_timeout: Duration::from_secs(1),
        })
        .log_batch(BatchOptions::default().max_batch_size(7))
        .metric_export_interval(Duration::from_secs(5))
        .runtime_metrics(true)
        .set_global(false)
        .connect_timeout(Duration::from_secs(1))
        .request_timeout(Duration::from_secs(2))
        .total_timeout(Duration::from_secs(9))
        .max_retries(1)
        .proxy("http://proxy:3128")
        .user_agent_suffix("it")
        .hooks(std::sync::Arc::new(NoHooks))
        .common(|c| c.max_retries(2))
        .build()
        .unwrap();
    assert_eq!(c.protocol(), Protocol::Grpc);
    assert_eq!(c.signal_endpoint(Signal::Metrics), "http://m:4317/");
    assert_eq!(c.signal_endpoint(Signal::Logs), "http://l:4318/custom");
    assert_eq!(c.signal_url(Signal::Traces).unwrap().path(), "/v1/traces");
    let h = c.signal_headers(Signal::Traces);
    assert_eq!(h["x-t"], "1");
    assert_eq!(h["x-common"], "c");
    assert_eq!(c.signal_headers(Signal::Logs)["x-t"], "common");
    assert_eq!(c.signal_only_headers(Signal::Logs).len(), 0);
    assert_eq!(c.compression(), Compression::None);
    assert_eq!(c.telemetry_url().unwrap().path(), "/metrics");
    assert_eq!(c.health_url().as_str(), "http://h:1/");
    assert!(!c.signal_enabled(Signal::Logs));
    assert!(c.signal_enabled(Signal::Traces));
    assert_eq!(c.common().retry().max_retries, 2);
    assert_eq!(c.common().proxy(), Some("http://proxy:3128"));
    let rm = c.resource_map();
    assert_eq!(rm["service.name"], json!("svc"));
    assert_eq!(rm["service.version"], json!("1.0"));
    assert_eq!(rm["deployment.environment.name"], json!("prod"));
    assert_eq!(rm["deployment.environment"], json!("prod"));
    assert_eq!(rm["team"], json!("core"));
    let tc = c.tracing_config();
    assert_eq!(tc.service_name, "svc");
    assert_eq!(tc.protocol, Protocol::Grpc);
    assert_eq!(tc.endpoint.as_deref(), Some("http://t:4318/v1/traces"));
    assert_eq!(tc.headers["x-t"], "1");
    assert_eq!(tc.sampler, SamplerKind::TraceIdRatio);
    assert_eq!(tc.batch.max_queue_size, 10);
    assert!(tc.tls.is_none());
    assert!(!tc.set_global);
    assert_eq!(
        tc.resource_attributes["deployment.environment.name"],
        json!("prod")
    );
    assert_eq!(c.log_batch().max_batch_size, 7);
    assert_eq!(c.metric_export_interval(), Duration::from_secs(5));
    // grpc endpoint without signal URL
    let g = Config::builder()
        .protocol(Protocol::Grpc)
        .grpc_url("http://g:4317/")
        .ca_cert_path("/x/ca.pem")
        .build()
        .unwrap();
    assert_eq!(g.signal_endpoint(Signal::Traces), "http://g:4317");
    assert_eq!(
        g.tracing_config().tls.unwrap().ca_cert_path,
        Some(PathBuf::from("/x/ca.pem"))
    );
}

struct NoHooks;
impl sdk::core::Hooks for NoHooks {}

#[test]
fn env_precedence_and_aliases() {
    // GROW_OPENTELEMETRY_COLLECTOR_* > GROW_OTEL_* > OTEL_*
    let c = from(&[
        ("GROW_OPENTELEMETRY_COLLECTOR_URL", "http://grow:1"),
        ("GROW_OTEL_URL", "http://alias:2"),
        ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://otel:3"),
    ]);
    assert_eq!(c.url().as_str(), "http://grow:1/");
    let c = from(&[
        ("GROW_OTEL_URL", "http://alias:2"),
        ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://otel:3"),
    ]);
    assert_eq!(c.url().as_str(), "http://alias:2/");
    let c = from(&[("OTEL_EXPORTER_OTLP_ENDPOINT", "http://otel:3")]);
    assert_eq!(c.url().as_str(), "http://otel:3/");
    assert_eq!(c.health_url().as_str(), "http://otel:13133/");
    // gRPC: the generic endpoint is the gRPC target, not the HTTP url.
    let c = from(&[
        ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
        ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://otel:4317"),
    ]);
    assert_eq!(c.protocol(), Protocol::Grpc);
    assert_eq!(c.grpc_url().as_str(), "http://otel:4317/");
    assert_eq!(c.url().as_str(), "http://127.0.0.1:4318/");
    let c = from(&[
        ("GROW_OTEL_PROTOCOL", "grpc"),
        ("GROW_OTEL_GRPC_URL", "http://alias:4317"),
    ]);
    assert_eq!(c.grpc_url().host_str(), Some("alias"));
    // Explicit protocol wins over the environment.
    let c = Config::builder()
        .protocol(Protocol::HttpProtobuf)
        .build_with_env(&env(&[("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc")]))
        .unwrap();
    assert_eq!(c.protocol(), Protocol::HttpProtobuf);
    let e = err(&[("OTEL_EXPORTER_OTLP_PROTOCOL", "http/json")]);
    assert_eq!(e.code(), ErrorCode::Config);
    assert!(e.message().contains("OTEL_EXPORTER_OTLP_PROTOCOL"));
    assert!(!e.message().contains("http/json"));
}

#[test]
fn env_otlp_settings() {
    let c = from(&[
        ("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", "http://t:1/custom"),
        ("GROW_OTEL_LOGS_URL", "http://l:2/v1/logs"),
        (
            "GROW_OPENTELEMETRY_COLLECTOR_METRICS_URL",
            "http://m:3/v1/metrics",
        ),
        (
            "OTEL_EXPORTER_OTLP_HEADERS",
            "Authorization=Bearer%20abc, x-a = 1 ,,",
        ),
        ("OTEL_EXPORTER_OTLP_TRACES_HEADERS", "x-t=2"),
        ("OTEL_EXPORTER_OTLP_LOGS_HEADERS", "x-l=3"),
        ("OTEL_EXPORTER_OTLP_METRICS_HEADERS", "x-m=4"),
        ("OTEL_EXPORTER_OTLP_COMPRESSION", "none"),
        ("OTEL_EXPORTER_OTLP_TIMEOUT", "2500"),
        ("GROW_OTEL_HEALTH_URL", "http://h:13133/"),
        ("GROW_OTEL_ZPAGES_URL", "http://z:55679"),
        ("GROW_OTEL_TELEMETRY_URL", "http://c:8888/metrics"),
    ]);
    assert_eq!(c.signal_endpoint(Signal::Traces), "http://t:1/custom");
    assert_eq!(c.signal_endpoint(Signal::Logs), "http://l:2/v1/logs");
    assert_eq!(c.signal_endpoint(Signal::Metrics), "http://m:3/v1/metrics");
    let h = c.signal_headers(Signal::Traces);
    assert_eq!(h["Authorization"], "Bearer abc");
    assert_eq!(h["x-a"], "1");
    assert_eq!(h["x-t"], "2");
    assert_eq!(c.signal_headers(Signal::Logs)["x-l"], "3");
    assert_eq!(c.signal_headers(Signal::Metrics)["x-m"], "4");
    assert_eq!(c.compression(), Compression::None);
    assert_eq!(c.export_timeout(), Duration::from_millis(2500));
    assert_eq!(c.trace_batch().export_timeout, Duration::from_millis(2500));
    assert_eq!(c.health_url().host_str(), Some("h"));
    assert_eq!(c.zpages_url().host_str(), Some("z"));
    assert!(c.telemetry_url().is_some());
    assert!(!format!("{c:?}").contains("abc"));
    // GROW headers win over OTEL headers; explicit headers win over both.
    let c = from(&[
        ("GROW_OTEL_HEADERS", "x-a=grow"),
        ("OTEL_EXPORTER_OTLP_HEADERS", "x-a=otel"),
    ]);
    assert_eq!(c.signal_headers(Signal::Traces)["x-a"], "grow");
    let c = Config::builder()
        .header("x-b", "explicit")
        .build_with_env(&env(&[("OTEL_EXPORTER_OTLP_HEADERS", "x-a=otel")]))
        .unwrap();
    assert!(!c.signal_headers(Signal::Traces).contains_key("x-a"));
    let c = Config::builder()
        .signal_header(Signal::Logs, "x-l", "explicit")
        .compression(Compression::Gzip)
        .build_with_env(&env(&[
            ("OTEL_EXPORTER_OTLP_LOGS_HEADERS", "x-l=env"),
            ("OTEL_EXPORTER_OTLP_COMPRESSION", "none"),
        ]))
        .unwrap();
    assert_eq!(c.signal_headers(Signal::Logs)["x-l"], "explicit");
    assert_eq!(c.compression(), Compression::Gzip);
    for (var, value) in [
        ("OTEL_EXPORTER_OTLP_HEADERS", "novalue"),
        ("OTEL_EXPORTER_OTLP_HEADERS", "=v"),
        ("OTEL_EXPORTER_OTLP_HEADERS", "k=%zz"),
        ("OTEL_EXPORTER_OTLP_LOGS_HEADERS", "bad"),
        ("OTEL_EXPORTER_OTLP_COMPRESSION", "zstd"),
        ("OTEL_EXPORTER_OTLP_TIMEOUT", "soon"),
        ("GROW_OTEL_HEALTH_URL", "not a url"),
    ] {
        let e = err(&[(var, value)]);
        assert_eq!(e.code(), ErrorCode::Config, "{var}");
    }
    assert_eq!(
        sdk::parse_key_values("a=%C3%A9", "V").unwrap(),
        vec![("a".to_owned(), "é".to_owned())]
    );
}

#[test]
fn env_tls_and_common_aliases() {
    let c = from(&[
        ("OTEL_EXPORTER_OTLP_CERTIFICATE", "/otel/ca.pem"),
        ("GROW_OTEL_CLIENT_CERT", "/alias/c.pem"),
        ("OTEL_EXPORTER_OTLP_CLIENT_KEY", "/otel/c.key"),
        ("GROW_OTEL_INSECURE_SKIP_VERIFY", "true"),
        ("GROW_OTEL_PROXY", "http://alias-proxy:1"),
        ("GROW_OPENTELEMETRY_COLLECTOR_CONNECT_TIMEOUT", "2s"),
    ]);
    let tls = c.common().tls();
    assert_eq!(tls.ca_cert_path, Some(PathBuf::from("/otel/ca.pem")));
    assert_eq!(tls.client_cert_path, Some(PathBuf::from("/alias/c.pem")));
    assert_eq!(tls.client_key_path, Some(PathBuf::from("/otel/c.key")));
    assert!(tls.insecure_skip_verify);
    assert_eq!(c.common().proxy(), Some("http://alias-proxy:1"));
    assert_eq!(c.common().connect_timeout(), Duration::from_secs(2));
    // GROW_<KEY>_ wins over the alias; explicit wins over both.
    let c = from(&[
        ("GROW_OPENTELEMETRY_COLLECTOR_CA_CERT", "/grow/ca.pem"),
        ("GROW_OTEL_CA_CERT", "/alias/ca.pem"),
    ]);
    assert_eq!(
        c.common().tls().ca_cert_path,
        Some(PathBuf::from("/grow/ca.pem"))
    );
    let c = Config::builder()
        .ca_cert_path("/explicit/ca.pem")
        .proxy("http://explicit:1")
        .build_with_env(&env(&[
            ("GROW_OTEL_CA_CERT", "/alias/ca.pem"),
            ("GROW_OTEL_PROXY", "http://alias:1"),
        ]))
        .unwrap();
    assert_eq!(
        c.common().tls().ca_cert_path,
        Some(PathBuf::from("/explicit/ca.pem"))
    );
    assert_eq!(c.common().proxy(), Some("http://explicit:1"));
    assert_eq!(
        err(&[("GROW_OTEL_INSECURE_SKIP_VERIFY", "maybe")]).code(),
        ErrorCode::Config
    );
    // An invalid explicit URL skips the alias pass and fails when building.
    let e = Config::builder()
        .url("::bad::")
        .build_with_env(&env(&[("GROW_OTEL_PROXY", "http://p:1")]))
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::Config);
}

#[test]
fn env_resource_and_signals() {
    let c = from(&[
        (
            "OTEL_RESOURCE_ATTRIBUTES",
            "service.name=from-attrs,team=core,k8s.ns=x",
        ),
        ("GROW_OTEL_SERVICE_VERSION", "2.0"),
        ("GROW_OTEL_ENVIRONMENT", "staging"),
        ("GROW_OTEL_RESOURCE_DETECTORS", "host, k8s,host"),
    ]);
    assert_eq!(c.service_name(), "from-attrs");
    assert_eq!(c.service_version(), Some("2.0"));
    assert_eq!(c.environment(), Some("staging"));
    assert_eq!(c.resource_attributes()["team"], json!("core"));
    assert_eq!(
        c.resource_detectors(),
        [ResourceDetector::Host, ResourceDetector::K8s]
    );
    // OTEL_SERVICE_NAME wins over service.name in OTEL_RESOURCE_ATTRIBUTES; explicit wins.
    let c = from(&[
        ("OTEL_RESOURCE_ATTRIBUTES", "service.name=from-attrs"),
        ("OTEL_SERVICE_NAME", "from-env"),
    ]);
    assert_eq!(c.service_name(), "from-env");
    let c = Config::builder()
        .resource_attribute("team", "explicit")
        .resource_attribute("service.name", "attr-explicit")
        .build_with_env(&env(&[(
            "OTEL_RESOURCE_ATTRIBUTES",
            "team=env,service.name=from-attrs",
        )]))
        .unwrap();
    assert_eq!(c.resource_attributes()["team"], json!("explicit"));
    assert!(c.service_name().starts_with("unknown_service:"));
    let c = from(&[("GROW_OTEL_RESOURCE_DETECTORS", "none")]);
    assert!(c.resource_detectors().is_empty());
    assert_eq!(
        err(&[("GROW_OTEL_RESOURCE_DETECTORS", "cloud")]).code(),
        ErrorCode::Config
    );
    assert_eq!(
        err(&[("OTEL_RESOURCE_ATTRIBUTES", "broken")]).code(),
        ErrorCode::Config
    );

    let c = from(&[("OTEL_SDK_DISABLED", "true"), ("GROW_OTEL_TRACES", "true")]);
    assert!(Signal::ALL.iter().all(|s| !c.signal_enabled(*s)));
    let c = Config::builder()
        .signal(Signal::Traces, true)
        .build_with_env(&env(&[("OTEL_SDK_DISABLED", "1")]))
        .unwrap();
    assert!(c.signal_enabled(Signal::Traces));
    let c = from(&[
        ("OTEL_TRACES_EXPORTER", "none"),
        ("OTEL_LOGS_EXPORTER", "otlp"),
        ("GROW_OPENTELEMETRY_COLLECTOR_METRICS", "false"),
        ("OTEL_METRICS_EXPORTER", "otlp"),
    ]);
    assert!(!c.signal_enabled(Signal::Traces));
    assert!(c.signal_enabled(Signal::Logs));
    assert!(!c.signal_enabled(Signal::Metrics));
    assert_eq!(
        err(&[("OTEL_SDK_DISABLED", "nah")]).code(),
        ErrorCode::Config
    );
    assert_eq!(err(&[("GROW_OTEL_LOGS", "nah")]).code(), ErrorCode::Config);
}

#[test]
fn env_tracing_and_batches() {
    let c = from(&[
        ("OTEL_TRACES_SAMPLER", "parentbased_traceidratio"),
        ("OTEL_TRACES_SAMPLER_ARG", "0.5"),
        ("OTEL_PROPAGATORS", "tracecontext,b3multi,none"),
        ("GROW_OTEL_RUNTIME_METRICS", "yes"),
        ("OTEL_BSP_SCHEDULE_DELAY", "200"),
        ("OTEL_BSP_EXPORT_TIMEOUT", "3000"),
        ("OTEL_BSP_MAX_QUEUE_SIZE", "100"),
        ("OTEL_BSP_MAX_EXPORT_BATCH_SIZE", "10"),
        ("OTEL_BLRP_SCHEDULE_DELAY", "300"),
        ("OTEL_BLRP_EXPORT_TIMEOUT", "4000"),
        ("OTEL_BLRP_MAX_QUEUE_SIZE", "50"),
        ("OTEL_BLRP_MAX_EXPORT_BATCH_SIZE", "5"),
        ("OTEL_METRIC_EXPORT_INTERVAL", "1500"),
    ]);
    assert_eq!(c.sampler(), SamplerKind::ParentBasedTraceIdRatio);
    assert_eq!(c.sampler_ratio(), 0.5);
    assert_eq!(
        c.propagators(),
        [PropagatorKind::TraceContext, PropagatorKind::B3Multi]
    );
    assert!(c.runtime_metrics());
    let b = c.trace_batch();
    assert_eq!(b.scheduled_delay, Duration::from_millis(200));
    assert_eq!(b.export_timeout, Duration::from_secs(3));
    assert_eq!((b.max_queue_size, b.max_export_batch_size), (100, 10));
    let l = c.log_batch();
    assert_eq!(l.flush_interval, Duration::from_millis(300));
    assert_eq!(l.export_timeout, Duration::from_secs(4));
    assert_eq!((l.max_queue_size, l.max_batch_size), (50, 5));
    assert_eq!(c.metric_export_interval(), Duration::from_millis(1500));
    for (var, value) in [
        ("OTEL_TRACES_SAMPLER", "sometimes"),
        ("OTEL_TRACES_SAMPLER_ARG", "2"),
        ("OTEL_TRACES_SAMPLER_ARG", "x"),
        ("OTEL_PROPAGATORS", ","),
        ("OTEL_BSP_MAX_QUEUE_SIZE", "-1"),
        ("OTEL_BSP_MAX_QUEUE_SIZE", "many"),
        ("OTEL_BSP_SCHEDULE_DELAY", "later"),
        ("OTEL_BLRP_MAX_EXPORT_BATCH_SIZE", "x"),
        ("OTEL_METRIC_EXPORT_INTERVAL", "x"),
        ("GROW_OTEL_RUNTIME_METRICS", "x"),
    ] {
        let r = Config::builder().build_with_env(&env(&[(var, value)]));
        if var == "OTEL_PROPAGATORS" {
            assert!(r.unwrap().propagators().is_empty());
            continue;
        }
        assert_eq!(r.unwrap_err().code(), ErrorCode::Config, "{var}={value}");
    }
    // Explicit batches win.
    let c = Config::builder()
        .trace_batch(TraceBatchConfig::default())
        .log_batch(BatchOptions::default())
        .build_with_env(&env(&[
            ("OTEL_BSP_MAX_QUEUE_SIZE", "100"),
            ("OTEL_BLRP_MAX_QUEUE_SIZE", "50"),
        ]))
        .unwrap();
    assert_eq!(c.trace_batch().max_queue_size, 2048);
    assert_eq!(c.log_batch().max_queue_size, 2048);
}

#[test]
fn validation() {
    let bad: Vec<(&str, sdk::Result<Config>)> = vec![
        (
            "native",
            Config::builder().protocol(Protocol::Native).build(),
        ),
        ("grpc url", Config::builder().grpc_url("grpc://x").build()),
        (
            "traces url",
            Config::builder().signal_url(Signal::Traces, "x").build(),
        ),
        (
            "health url",
            Config::builder().health_url("ftp://x").build(),
        ),
        ("zpages url", Config::builder().zpages_url("/rel").build()),
        ("telemetry", Config::builder().telemetry_url("nope").build()),
        (
            "export timeout",
            Config::builder().export_timeout(Duration::ZERO).build(),
        ),
        (
            "interval",
            Config::builder()
                .metric_export_interval(Duration::ZERO)
                .build(),
        ),
        (
            "ratio",
            Config::builder()
                .sampler(SamplerKind::TraceIdRatio, 2.0)
                .build(),
        ),
        (
            "log batch",
            Config::builder()
                .log_batch(BatchOptions::default().max_queue_size(0))
                .build(),
        ),
    ];
    for (name, r) in bad {
        assert_eq!(r.unwrap_err().code(), ErrorCode::Config, "{name}");
    }
    let e = Config::builder()
        .protocol(Protocol::Native)
        .build()
        .unwrap_err();
    assert!(e.message().contains("native"));
    // Empty optional values count as unset.
    let c = Config::builder()
        .signal_url(Signal::Logs, "")
        .service_name(" ")
        .service_version("")
        .environment("")
        .build()
        .unwrap();
    assert!(c.signal_url(Signal::Logs).is_none());
    assert!(c.service_name().starts_with("unknown_service:"));
    assert!(c.validate().is_ok());
    let _ = Config::from_env();
    assert!(!format!("{:?}", Config::builder().grpc_url("http://u:pw-1@g")).contains("pw-1"));
}
