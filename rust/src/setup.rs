// Service code (OpenTelemetry Collector): `init_tracing` with the collector's OTLP defaults.
//! `init_tracing` for the collector: OTLP `http/protobuf` to `http://127.0.0.1:4318/v1/traces`
//! by default (or OTLP gRPC to `http://127.0.0.1:4317`), defaults from
//! [`OtelCollectorConfig`] (environment), resource detection and the official exporter factory.

use std::collections::HashMap;

use opentelemetry_otlp::{SpanExporter, WithExportConfig, WithHttpConfig, WithTonicConfig};

use crate::config::{native_error, OtelCollectorConfig};
use crate::core::{EnvReader, Error, Result, TlsConfig};
use crate::otlp::{grpc_metadata, reqwest_client, tonic_tls, with_tonic_tls, ClientSettings};
use crate::resource::detect_resource_attributes;
use crate::tracing::{
    init_tracing_with, Compression, PropagatorKind, Protocol, SamplerKind, TraceBatchConfig,
    TracingConfig, TracingHandle,
};

/// Default OTLP/HTTP traces endpoint of a collector.
pub const DEFAULT_OTLP_HTTP_TRACES_ENDPOINT: &str = "http://127.0.0.1:4318/v1/traces";

fn has_path(endpoint: &str) -> bool {
    url::Url::parse(endpoint)
        .map(|u| u.path() != "/" && !u.path().is_empty())
        .unwrap_or(true)
}

/// The exporter URL for a protocol: for `http/protobuf`, an endpoint without a path (a base such
/// as `http://collector:4318`) gets `/v1/traces`; any other endpoint is used as is. Without an
/// endpoint, the collector default for the protocol.
pub fn resolve_traces_endpoint(protocol: Protocol, endpoint: Option<&str>) -> String {
    match (protocol, endpoint.filter(|e| !e.is_empty())) {
        (Protocol::Grpc, Some(e)) => e.to_owned(),
        (Protocol::Grpc, None) => crate::config::DEFAULT_GRPC_URL.to_owned(),
        (_, Some(e)) if has_path(e) => e.to_owned(),
        (_, Some(e)) => format!("{}/v1/traces", e.trim_end_matches('/')),
        (_, None) => DEFAULT_OTLP_HTTP_TRACES_ENDPOINT.to_owned(),
    }
}

/// Fills every unset field of `explicit` from the collector config: endpoint (an explicit
/// endpoint without a path gets `/v1/traces` for `http/protobuf`; otherwise the traces URL, `url
/// + /v1/traces` or the gRPC target by protocol), protocol (a field still at its
/// [`TracingConfig::new`] default counts as unset), service name/version/environment,
/// compression, TLS, sampler, propagators and batch fields. Headers and resource attributes
/// merge key by key (explicit wins); `set_global` is off when either side turns it off.
/// `native` is `CONFIG`: the collector speaks OTLP only.
pub fn tracing_config_from(
    explicit: TracingConfig,
    collector: &OtelCollectorConfig,
) -> Result<TracingConfig> {
    if explicit.protocol == Protocol::Native {
        return Err(native_error());
    }
    let defaults = collector.tracing_config();
    let blank = TracingConfig::new("");
    let mut out = explicit.clone();
    if explicit.protocol == blank.protocol {
        out.protocol = defaults.protocol;
    }
    out.endpoint = Some(match &explicit.endpoint {
        Some(e) => resolve_traces_endpoint(out.protocol, Some(e)),
        None if out.protocol != defaults.protocol => match out.protocol {
            Protocol::Grpc => collector
                .grpc_url()
                .as_str()
                .trim_end_matches('/')
                .to_owned(),
            _ => format!(
                "{}/v1/traces",
                collector.url().as_str().trim_end_matches('/')
            ),
        },
        None => defaults.endpoint.clone().unwrap_or_default(),
    });
    if out.service_name.trim().is_empty() {
        out.service_name = defaults.service_name.clone();
    }
    if out.service_version.is_none() {
        out.service_version = defaults.service_version.clone();
    }
    if out.environment.is_none() {
        out.environment = defaults.environment.clone();
    }
    if explicit.compression == blank.compression {
        out.compression = defaults.compression;
    }
    let mut headers = defaults.headers.clone();
    headers.extend(explicit.headers.clone());
    out.headers = headers;
    let mut attrs = defaults.resource_attributes.clone();
    attrs.extend(explicit.resource_attributes.clone());
    out.resource_attributes = attrs;
    if explicit.sampler == SamplerKind::default() && explicit.sampler_ratio == 1.0 {
        out.sampler = defaults.sampler;
        out.sampler_ratio = defaults.sampler_ratio;
    }
    if explicit.propagators == [PropagatorKind::TraceContext, PropagatorKind::Baggage] {
        out.propagators = defaults.propagators.clone();
    }
    if out.tls.is_none() {
        out.tls = defaults.tls.clone();
    }
    let d = TraceBatchConfig::default();
    if explicit.batch.max_queue_size == d.max_queue_size {
        out.batch.max_queue_size = defaults.batch.max_queue_size;
    }
    if explicit.batch.max_export_batch_size == d.max_export_batch_size {
        out.batch.max_export_batch_size = defaults.batch.max_export_batch_size;
    }
    if explicit.batch.scheduled_delay == d.scheduled_delay {
        out.batch.scheduled_delay = defaults.batch.scheduled_delay;
    }
    if explicit.batch.export_timeout == d.export_timeout {
        out.batch.export_timeout = defaults.batch.export_timeout;
    }
    out.set_global = explicit.set_global && defaults.set_global;
    Ok(out)
}

fn collector_from_env(explicit: &TracingConfig, env: &EnvReader) -> Result<OtelCollectorConfig> {
    if explicit.protocol == Protocol::Native {
        return Err(native_error());
    }
    let mut b = OtelCollectorConfig::builder();
    if explicit.protocol == Protocol::Grpc {
        b = b.protocol(Protocol::Grpc);
    }
    b.build_with_env(env)
}

/// [`tracing_config_from`] with the collector config read from `env` (see
/// [`OtelCollectorConfig`] for the variables; an explicit `grpc` protocol selects the gRPC
/// endpoint variables).
pub fn tracing_config_from_env(explicit: TracingConfig, env: &EnvReader) -> Result<TracingConfig> {
    let collector = collector_from_env(&explicit, env)?;
    tracing_config_from(explicit, &collector)
}

/// Builds the official OTLP span exporter for the collector (the exporter factory of
/// [`init_tracing_with`]): `http/protobuf` posts protobuf to the resolved endpoint (see
/// [`resolve_traces_endpoint`]), `grpc` uses tonic. Headers, compression, timeout
/// (`batch.export_timeout`) and TLS (CA, client certificate, `insecure_skip_verify` over HTTP)
/// come from the config. `native` is `CONFIG`, and so is `insecure_skip_verify` over gRPC
/// (tonic cannot skip verification). The environment is not read here.
pub fn collector_exporter(config: &TracingConfig) -> Result<SpanExporter> {
    let endpoint = resolve_traces_endpoint(config.protocol, config.endpoint.as_deref());
    let tls = config.tls.clone().unwrap_or_default();
    let gzip = config.compression == Compression::Gzip;
    let built = match config.protocol {
        Protocol::Native => return Err(native_error()),
        Protocol::HttpProtobuf => {
            let mut b = SpanExporter::builder()
                .with_http()
                .with_endpoint(endpoint)
                .with_timeout(config.batch.export_timeout)
                .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
                .with_headers(
                    config
                        .headers
                        .clone()
                        .into_iter()
                        .collect::<HashMap<_, _>>(),
                );
            if gzip {
                b = b.with_compression(opentelemetry_otlp::Compression::Gzip);
            }
            if tls != TlsConfig::default() {
                b = b.with_http_client(reqwest_client(&ClientSettings {
                    tls,
                    proxy: None,
                    connect_timeout: None,
                    timeout: config.batch.export_timeout,
                })?);
            }
            b.build()
        }
        Protocol::Grpc => {
            let mut b = SpanExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint.clone())
                .with_timeout(config.batch.export_timeout)
                .with_metadata(grpc_metadata(&config.headers)?);
            if gzip {
                b = b.with_compression(opentelemetry_otlp::Compression::Gzip);
            }
            b = with_tonic_tls(b, tonic_tls(&endpoint, &tls)?);
            b.build()
        }
    };
    built.map_err(|e| Error::config(format!("tracing: cannot build the OTLP exporter: {e}")))
}

/// Adds the detected resource attributes (explicit keys win) and
/// `deployment.environment.name`.
fn with_detected(mut config: TracingConfig, collector: &OtelCollectorConfig) -> TracingConfig {
    let mut attrs = detect_resource_attributes(collector.resource_detectors());
    attrs.extend(std::mem::take(&mut config.resource_attributes));
    if let Some(env) = &config.environment {
        attrs
            .entry("deployment.environment.name".into())
            .or_insert_with(|| env.clone().into());
    }
    config.resource_attributes = attrs;
    config
}

/// Sets up OpenTelemetry tracing that exports spans to the collector over OTLP (SPEC §5):
/// `http/protobuf` to `http://127.0.0.1:4318/v1/traces` unless configured otherwise (see
/// [`tracing_config_from_env`] for how the process environment fills unset fields). It builds
/// the official exporter, a batch span processor, the sampler, the detected resource (host,
/// process, container, Kubernetes) and the propagators, and installs them globally unless
/// `set_global` is off. Must be called inside a tokio runtime; call
/// [`TracingHandle::shutdown`] before exit.
pub fn init_tracing(config: TracingConfig) -> Result<TracingHandle> {
    init_tracing_with_env(config, &EnvReader::new())
}

/// [`init_tracing`] reading `env` instead of the process environment.
pub fn init_tracing_with_env(config: TracingConfig, env: &EnvReader) -> Result<TracingHandle> {
    let collector = collector_from_env(&config, env)?;
    init_tracing_with_collector(config, &collector)
}

/// [`init_tracing`] with `collector` as the defaults of `config` (see [`tracing_config_from`]);
/// the environment is not read.
pub fn init_tracing_with_collector(
    config: TracingConfig,
    collector: &OtelCollectorConfig,
) -> Result<TracingHandle> {
    let resolved = with_detected(tracing_config_from(config, collector)?, collector);
    init_tracing_with(resolved, collector_exporter)
}
