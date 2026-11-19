// Service code (OpenTelemetry Collector): configuration of the client and of `init_telemetry`.
//! `OtelCollectorConfig`: common settings (SPEC §2.2) plus the OTLP exporter settings of the
//! three signals, the collector's admin endpoints (health_check, zpages, internal telemetry), the
//! shared resource, sampling, propagation and batching.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use url::Url;

use crate::core::redact::{is_sensitive_header, redact_headers, redact_url};
use crate::core::{
    parse_bool, CommonConfig, CommonConfigBuilder, EnvReader, Error, Hooks, Result, RetryPolicy,
    Secret, TlsConfig,
};
use crate::logging::{AttrValue, Attributes, BatchOptions};
use crate::resource::ResourceDetector;
use crate::tracing::{
    Compression, PropagatorKind, Protocol, SamplerKind, TraceBatchConfig, TracingConfig,
};

/// OTLP/HTTP base URL used when neither the builder nor the environment sets one (port 4318).
pub const DEFAULT_URL: &str = "http://127.0.0.1:4318";
/// OTLP/gRPC target used when neither the builder nor the environment sets one (port 4317).
pub const DEFAULT_GRPC_URL: &str = "http://127.0.0.1:4317";
/// Port of the collector's `health_check` extension.
pub const DEFAULT_HEALTH_PORT: u16 = 13133;
/// Port of the collector's `zpages` extension.
pub const DEFAULT_ZPAGES_PORT: u16 = 55679;
/// Default export protocol: OTLP over HTTP with protobuf payloads.
pub const DEFAULT_PROTOCOL: Protocol = Protocol::HttpProtobuf;
/// Default deadline of one OTLP export (the OpenTelemetry default, 10 s).
pub const DEFAULT_EXPORT_TIMEOUT: Duration = Duration::from_secs(10);
/// Default interval of the periodic metric reader (60 s).
pub const DEFAULT_METRIC_EXPORT_INTERVAL: Duration = Duration::from_secs(60);
/// Short environment prefix accepted after `GROW_OPENTELEMETRY_COLLECTOR_` (lower precedence).
pub const ALIAS_ENV_PREFIX: &str = "GROW_OTEL_";

/// An OpenTelemetry signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Signal {
    /// Spans.
    Traces,
    /// Log records.
    Logs,
    /// Metrics.
    Metrics,
}

impl Signal {
    /// Every signal.
    pub const ALL: [Signal; 3] = [Signal::Traces, Signal::Logs, Signal::Metrics];

    /// Wire name: `traces`, `logs`, `metrics`.
    pub fn as_str(self) -> &'static str {
        match self {
            Signal::Traces => "traces",
            Signal::Logs => "logs",
            Signal::Metrics => "metrics",
        }
    }

    fn env_name(self) -> &'static str {
        match self {
            Signal::Traces => "TRACES",
            Signal::Logs => "LOGS",
            Signal::Metrics => "METRICS",
        }
    }
}

/// Immutable configuration of [`OtelCollectorClient`](crate::OtelCollectorClient) and
/// [`init_telemetry`](crate::init_telemetry). Build it with [`OtelCollectorConfig::builder`] or
/// [`OtelCollectorConfig::from_env`]; `Debug` output redacts every credential.
///
/// The common `url` is the OTLP/HTTP base URL (default [`DEFAULT_URL`]); the common headers are
/// sent with every export and with `ping()` (put gateway credentials there, they are redacted
/// everywhere).
///
/// Environment variables read by [`OtelCollectorConfig::from_env`] (explicit value >
/// `GROW_OPENTELEMETRY_COLLECTOR_*` > `GROW_OTEL_*` > per-signal `OTEL_*` > generic `OTEL_*` >
/// default; the table lists the suffix after the prefix):
///
/// | Setting | Variables |
/// | - | - |
/// | `url` | `URL`, `OTEL_EXPORTER_OTLP_ENDPOINT` (protocol `http/protobuf` only) |
/// | `grpc_url` | `GRPC_URL`, `OTEL_EXPORTER_OTLP_ENDPOINT` (protocol `grpc` only) |
/// | `protocol` | `PROTOCOL`, `OTEL_EXPORTER_OTLP_PROTOCOL` (`grpc` or `http/protobuf`) |
/// | `traces_url`, `logs_url`, `metrics_url` | `TRACES_URL`, `LOGS_URL`, `METRICS_URL`, `OTEL_EXPORTER_OTLP_<SIGNAL>_ENDPOINT` (used as is) |
/// | headers | `HEADERS`, `OTEL_EXPORTER_OTLP_HEADERS` (`k=v,k2=v2`, URL-encoded values) |
/// | per-signal headers | `OTEL_EXPORTER_OTLP_<SIGNAL>_HEADERS` (merged over the headers) |
/// | `compression` | `COMPRESSION`, `OTEL_EXPORTER_OTLP_COMPRESSION` (`gzip` or `none`) |
/// | `export_timeout` | `EXPORT_TIMEOUT`, `OTEL_EXPORTER_OTLP_TIMEOUT` (integer ms) |
/// | TLS | `CA_CERT`, `CLIENT_CERT`, `CLIENT_KEY`, `OTEL_EXPORTER_OTLP_CERTIFICATE`, `OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE`, `OTEL_EXPORTER_OTLP_CLIENT_KEY` |
/// | `health_url` | `HEALTH_URL` (default: `url` host, port 13133, path `/`) |
/// | `zpages_url` | `ZPAGES_URL` (default: `url` host, port 55679) |
/// | `telemetry_url` | `TELEMETRY_URL` (optional, the collector's `:8888/metrics`) |
/// | `service_name` | `SERVICE_NAME`, `OTEL_SERVICE_NAME`, `service.name` in `OTEL_RESOURCE_ATTRIBUTES` |
/// | `service_version`, `environment` | `SERVICE_VERSION`, `ENVIRONMENT` |
/// | `resource_attributes` | `RESOURCE_ATTRIBUTES`, `OTEL_RESOURCE_ATTRIBUTES` (explicit keys win) |
/// | `resource_detectors` | `RESOURCE_DETECTORS` (`host,process,container,k8s` or `none`) |
/// | signals on/off | `TRACES`, `LOGS`, `METRICS` (bool), `OTEL_<SIGNAL>_EXPORTER=none`, `OTEL_SDK_DISABLED=true` |
/// | `sampler`, `sampler_ratio` | `SAMPLER`, `OTEL_TRACES_SAMPLER`; `SAMPLER_RATIO`, `OTEL_TRACES_SAMPLER_ARG` |
/// | `propagators` | `PROPAGATORS`, `OTEL_PROPAGATORS` |
/// | `trace_batch` | `OTEL_BSP_SCHEDULE_DELAY`, `OTEL_BSP_EXPORT_TIMEOUT`, `OTEL_BSP_MAX_QUEUE_SIZE`, `OTEL_BSP_MAX_EXPORT_BATCH_SIZE` |
/// | `log_batch` | `OTEL_BLRP_SCHEDULE_DELAY`, `OTEL_BLRP_EXPORT_TIMEOUT`, `OTEL_BLRP_MAX_QUEUE_SIZE`, `OTEL_BLRP_MAX_EXPORT_BATCH_SIZE` |
/// | `metric_export_interval` | `METRIC_EXPORT_INTERVAL`, `OTEL_METRIC_EXPORT_INTERVAL` (ms) |
/// | `runtime_metrics` | `RUNTIME_METRICS` (bool) |
///
/// plus the common `CONNECT_TIMEOUT`, `REQUEST_TIMEOUT`, `TOTAL_TIMEOUT`, `MAX_RETRIES`,
/// `INSECURE_SKIP_VERIFY` and `PROXY`. A present but invalid value is `CONFIG` naming the
/// variable, never its value.
#[derive(Clone)]
pub struct OtelCollectorConfig {
    common: CommonConfig,
    probe: CommonConfig,
    grpc_url: Url,
    protocol: Protocol,
    traces_url: Option<Url>,
    logs_url: Option<Url>,
    metrics_url: Option<Url>,
    traces_headers: BTreeMap<String, String>,
    logs_headers: BTreeMap<String, String>,
    metrics_headers: BTreeMap<String, String>,
    compression: Compression,
    export_timeout: Duration,
    health_url: Url,
    zpages_url: Url,
    telemetry_url: Option<Url>,
    service_name: String,
    service_version: Option<String>,
    environment: Option<String>,
    resource_attributes: Attributes,
    resource_detectors: Vec<ResourceDetector>,
    traces: bool,
    logs: bool,
    metrics: bool,
    sampler: SamplerKind,
    sampler_ratio: f64,
    propagators: Vec<PropagatorKind>,
    trace_batch: TraceBatchConfig,
    log_batch: BatchOptions,
    metric_export_interval: Duration,
    runtime_metrics: bool,
    set_global: bool,
}

impl fmt::Debug for OtelCollectorConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let url = |u: &Url| redact_url(u.as_str());
        f.debug_struct("OtelCollectorConfig")
            .field("common", &self.common)
            .field("grpc_url", &url(&self.grpc_url))
            .field("protocol", &self.protocol)
            .field("traces_url", &self.traces_url.as_ref().map(url))
            .field("logs_url", &self.logs_url.as_ref().map(url))
            .field("metrics_url", &self.metrics_url.as_ref().map(url))
            .field(
                "traces_headers",
                &redact_headers(self.traces_headers.iter()),
            )
            .field("logs_headers", &redact_headers(self.logs_headers.iter()))
            .field(
                "metrics_headers",
                &redact_headers(self.metrics_headers.iter()),
            )
            .field("compression", &self.compression)
            .field("export_timeout", &self.export_timeout)
            .field("health_url", &url(&self.health_url))
            .field("zpages_url", &url(&self.zpages_url))
            .field("telemetry_url", &self.telemetry_url.as_ref().map(url))
            .field("service_name", &self.service_name)
            .field("service_version", &self.service_version)
            .field("environment", &self.environment)
            .field("resource_attributes", &self.resource_attributes)
            .field("resource_detectors", &self.resource_detectors)
            .field("traces", &self.traces)
            .field("logs", &self.logs)
            .field("metrics", &self.metrics)
            .field("sampler", &self.sampler)
            .field("sampler_ratio", &self.sampler_ratio)
            .field("propagators", &self.propagators)
            .field("trace_batch", &self.trace_batch)
            .field("log_batch", &self.log_batch)
            .field("metric_export_interval", &self.metric_export_interval)
            .field("runtime_metrics", &self.runtime_metrics)
            .field("set_global", &self.set_global)
            .finish()
    }
}

impl OtelCollectorConfig {
    /// A builder with nothing set.
    pub fn builder() -> OtelCollectorConfigBuilder {
        OtelCollectorConfigBuilder::default()
    }

    /// Configuration from the process environment (see the table on [`OtelCollectorConfig`]).
    pub fn from_env() -> Result<Self> {
        Self::builder().build_from_env()
    }

    /// Settings shared by every Grow Codes SDK.
    pub fn common(&self) -> &CommonConfig {
        &self.common
    }

    /// The common settings without retries (health and version probes never retry).
    pub(crate) fn probe(&self) -> &CommonConfig {
        &self.probe
    }

    /// OTLP/HTTP base URL.
    pub fn url(&self) -> &Url {
        self.common.url()
    }

    /// OTLP/gRPC target (`https://` means TLS).
    pub fn grpc_url(&self) -> &Url {
        &self.grpc_url
    }

    /// Export protocol (`grpc` or `http/protobuf`).
    pub fn protocol(&self) -> Protocol {
        self.protocol
    }

    /// Explicit endpoint of one signal (used as is), when set.
    pub fn signal_url(&self, signal: Signal) -> Option<&Url> {
        match signal {
            Signal::Traces => self.traces_url.as_ref(),
            Signal::Logs => self.logs_url.as_ref(),
            Signal::Metrics => self.metrics_url.as_ref(),
        }
    }

    /// Export endpoint of a signal: the per-signal URL when set, else for `http/protobuf`
    /// `url + /v1/<signal>` and for `grpc` the gRPC target.
    pub fn signal_endpoint(&self, signal: Signal) -> String {
        if let Some(u) = self.signal_url(signal) {
            return u.to_string();
        }
        match self.protocol {
            Protocol::Grpc => self.grpc_url.to_string().trim_end_matches('/').to_owned(),
            _ => format!(
                "{}/v1/{}",
                self.url().as_str().trim_end_matches('/'),
                signal.as_str()
            ),
        }
    }

    /// Extra headers of one signal only (merged over the common headers on export).
    pub fn signal_only_headers(&self, signal: Signal) -> &BTreeMap<String, String> {
        match signal {
            Signal::Traces => &self.traces_headers,
            Signal::Logs => &self.logs_headers,
            Signal::Metrics => &self.metrics_headers,
        }
    }

    /// Headers sent with exports of a signal: the common headers merged with the signal's own
    /// headers (signal values win per key).
    pub fn signal_headers(&self, signal: Signal) -> BTreeMap<String, String> {
        let mut out: BTreeMap<String, String> = self
            .common
            .headers()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (k, v) in self.signal_only_headers(signal) {
            out.insert(k.clone(), v.clone());
        }
        out
    }

    /// Export compression (default gzip).
    pub fn compression(&self) -> Compression {
        self.compression
    }

    /// Deadline of one OTLP export (default 10 s).
    pub fn export_timeout(&self) -> Duration {
        self.export_timeout
    }

    /// The `health_check` extension URL (default: `url` host, port 13133, path `/`).
    pub fn health_url(&self) -> &Url {
        &self.health_url
    }

    /// The `zpages` extension base URL (default: `url` host, port 55679).
    pub fn zpages_url(&self) -> &Url {
        &self.zpages_url
    }

    /// The collector's own Prometheus endpoint (`:8888/metrics`), when configured;
    /// `server_version()` falls back to its `target_info` metric.
    pub fn telemetry_url(&self) -> Option<&Url> {
        self.telemetry_url.as_ref()
    }

    /// `service.name` (default `unknown_service:<executable name>`).
    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    /// `service.version`, when set.
    pub fn service_version(&self) -> Option<&str> {
        self.service_version.as_deref()
    }

    /// `deployment.environment.name`, when set.
    pub fn environment(&self) -> Option<&str> {
        self.environment.as_deref()
    }

    /// Extra resource attributes.
    pub fn resource_attributes(&self) -> &Attributes {
        &self.resource_attributes
    }

    /// Resource detectors of `init_telemetry` and `init_tracing` (default all four).
    pub fn resource_detectors(&self) -> &[ResourceDetector] {
        &self.resource_detectors
    }

    /// Whether a signal is enabled in `init_telemetry` (default all).
    pub fn signal_enabled(&self, signal: Signal) -> bool {
        match signal {
            Signal::Traces => self.traces,
            Signal::Logs => self.logs,
            Signal::Metrics => self.metrics,
        }
    }

    /// Head sampler (default `parentbased_always_on`).
    pub fn sampler(&self) -> SamplerKind {
        self.sampler
    }

    /// Ratio of the ratio samplers (default 1.0).
    pub fn sampler_ratio(&self) -> f64 {
        self.sampler_ratio
    }

    /// Propagators (default `tracecontext`, `baggage`).
    pub fn propagators(&self) -> &[PropagatorKind] {
        &self.propagators
    }

    /// Span batch processor settings (export timeout defaults to `export_timeout`).
    pub fn trace_batch(&self) -> TraceBatchConfig {
        self.trace_batch
    }

    /// Log batch settings of log clients created from this config.
    pub fn log_batch(&self) -> &BatchOptions {
        &self.log_batch
    }

    /// Interval of the periodic metric reader (default 60 s).
    pub fn metric_export_interval(&self) -> Duration {
        self.metric_export_interval
    }

    /// Whether `init_telemetry` registers runtime metrics (default `false`).
    pub fn runtime_metrics(&self) -> bool {
        self.runtime_metrics
    }

    /// Whether `init_telemetry` / `init_tracing` install the providers globally (default
    /// `true`).
    pub fn set_global(&self) -> bool {
        self.set_global
    }

    /// The configured resource as attributes: `resource_attributes` plus `service.name`,
    /// `service.version`, `deployment.environment.name` and `deployment.environment`.
    pub fn resource_map(&self) -> Attributes {
        let mut out = self.resource_attributes.clone();
        out.insert(
            "service.name".into(),
            AttrValue::String(self.service_name.clone()),
        );
        if let Some(v) = &self.service_version {
            out.insert("service.version".into(), AttrValue::String(v.clone()));
        }
        if let Some(e) = &self.environment {
            out.insert(
                "deployment.environment.name".into(),
                AttrValue::String(e.clone()),
            );
            out.insert(
                "deployment.environment".into(),
                AttrValue::String(e.clone()),
            );
        }
        out
    }

    /// The [`TracingConfig`] of the traces signal (endpoint, protocol, headers, TLS,
    /// compression, resource, sampler, propagators, batch, `set_global`).
    pub fn tracing_config(&self) -> TracingConfig {
        let mut c = TracingConfig::new(self.service_name.clone());
        c.service_version = self.service_version.clone();
        c.environment = self.environment.clone();
        c.resource_attributes = self.resource_attributes.clone();
        if let Some(e) = &self.environment {
            c.resource_attributes
                .entry("deployment.environment.name".into())
                .or_insert_with(|| AttrValue::String(e.clone()));
        }
        c.endpoint = Some(self.signal_endpoint(Signal::Traces));
        c.protocol = self.protocol;
        c.headers = self.signal_headers(Signal::Traces);
        c.sampler = self.sampler;
        c.sampler_ratio = self.sampler_ratio;
        c.propagators = self.propagators.clone();
        c.batch = self.trace_batch;
        let tls = self.common.tls();
        c.tls = (tls != &TlsConfig::default()).then(|| tls.clone());
        c.compression = self.compression;
        c.set_global = self.set_global;
        c
    }

    /// Credential values in the configured headers (registered with every transport so they
    /// never appear in errors or hooks).
    pub(crate) fn known_secrets(&self) -> Vec<Secret> {
        let mut out = Vec::new();
        let common = self.common.headers().iter().map(|(k, v)| (k, v));
        let signals = Signal::ALL
            .iter()
            .flat_map(|s| self.signal_only_headers(*s).iter());
        for (k, v) in common.chain(signals) {
            if is_sensitive_header(k) && !v.is_empty() {
                out.push(Secret::new(v.clone()));
                if let Some((_, token)) = v.split_once(' ') {
                    out.push(Secret::new(token.to_owned()));
                }
            }
        }
        out
    }

    /// Checks the service rules (they are also checked when building).
    pub fn validate(&self) -> Result<()> {
        if self.protocol == Protocol::Native {
            return Err(native_error());
        }
        if self.export_timeout.is_zero() || self.metric_export_interval.is_zero() {
            return Err(Error::config(
                "exportTimeout and metricExportInterval must be positive",
            ));
        }
        self.log_batch.validate()?;
        let common = self.common.headers().iter().map(|(k, v)| (k, v));
        let signals = Signal::ALL
            .iter()
            .flat_map(|s| self.signal_only_headers(*s).iter());
        for (k, v) in common.chain(signals) {
            if http::header::HeaderName::from_bytes(k.as_bytes()).is_err() {
                return Err(Error::config(format!("invalid header name {k:?}")));
            }
            if http::header::HeaderValue::from_str(v).is_err() {
                return Err(Error::config(format!("invalid value for header {k:?}")));
            }
        }
        self.tracing_config().validate()
    }
}

pub(crate) fn native_error() -> Error {
    Error::config(
        "protocol native is not supported by the OpenTelemetry Collector; use grpc or http/protobuf",
    )
}

/// Builder for [`OtelCollectorConfig`]. Values set here win over the environment. `Debug`
/// output redacts every credential.
#[derive(Clone, Default)]
pub struct OtelCollectorConfigBuilder {
    common: CommonConfigBuilder,
    grpc_url: Option<String>,
    protocol: Option<Protocol>,
    traces_url: Option<String>,
    logs_url: Option<String>,
    metrics_url: Option<String>,
    traces_headers: BTreeMap<String, String>,
    logs_headers: BTreeMap<String, String>,
    metrics_headers: BTreeMap<String, String>,
    compression: Option<Compression>,
    export_timeout: Option<Duration>,
    health_url: Option<String>,
    zpages_url: Option<String>,
    telemetry_url: Option<String>,
    service_name: Option<String>,
    service_version: Option<String>,
    environment: Option<String>,
    resource_attributes: Attributes,
    resource_detectors: Option<Vec<ResourceDetector>>,
    traces: Option<bool>,
    logs: Option<bool>,
    metrics: Option<bool>,
    sampler: Option<SamplerKind>,
    sampler_ratio: Option<f64>,
    propagators: Option<Vec<PropagatorKind>>,
    trace_batch: Option<TraceBatchConfig>,
    log_batch: Option<BatchOptions>,
    metric_export_interval: Option<Duration>,
    runtime_metrics: Option<bool>,
    set_global: Option<bool>,
}

impl fmt::Debug for OtelCollectorConfigBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let url = |u: &Option<String>| u.as_deref().map(redact_url);
        f.debug_struct("OtelCollectorConfigBuilder")
            .field("common", &self.common)
            .field("grpc_url", &url(&self.grpc_url))
            .field("protocol", &self.protocol)
            .field("traces_url", &url(&self.traces_url))
            .field("logs_url", &url(&self.logs_url))
            .field("metrics_url", &url(&self.metrics_url))
            .field(
                "traces_headers",
                &redact_headers(self.traces_headers.iter()),
            )
            .field("logs_headers", &redact_headers(self.logs_headers.iter()))
            .field(
                "metrics_headers",
                &redact_headers(self.metrics_headers.iter()),
            )
            .field("service_name", &self.service_name)
            .finish_non_exhaustive()
    }
}

impl OtelCollectorConfigBuilder {
    /// OTLP/HTTP base URL (default [`DEFAULT_URL`]); the signal paths `/v1/<signal>` are
    /// appended to it.
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.common = self.common.url(url);
        self
    }
    /// OTLP/gRPC target (default [`DEFAULT_GRPC_URL`]; `https://` means TLS).
    pub fn grpc_url(mut self, url: impl Into<String>) -> Self {
        self.grpc_url = Some(url.into());
        self
    }
    /// Export protocol (default `http/protobuf`; `native` is a `CONFIG` error).
    pub fn protocol(mut self, protocol: Protocol) -> Self {
        self.protocol = Some(protocol);
        self
    }
    /// Endpoint of one signal, used as is (HTTP: no path is appended; gRPC: the target).
    pub fn signal_url(mut self, signal: Signal, url: impl Into<String>) -> Self {
        let url = Some(url.into());
        match signal {
            Signal::Traces => self.traces_url = url,
            Signal::Logs => self.logs_url = url,
            Signal::Metrics => self.metrics_url = url,
        }
        self
    }
    /// Header sent with exports of one signal only (signal values win over the common headers).
    pub fn signal_header(
        mut self,
        signal: Signal,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        let map = match signal {
            Signal::Traces => &mut self.traces_headers,
            Signal::Logs => &mut self.logs_headers,
            Signal::Metrics => &mut self.metrics_headers,
        };
        map.insert(name.into(), value.into());
        self
    }
    /// Export compression (default gzip).
    pub fn compression(mut self, compression: Compression) -> Self {
        self.compression = Some(compression);
        self
    }
    /// Deadline of one OTLP export (default 10 s).
    pub fn export_timeout(mut self, timeout: Duration) -> Self {
        self.export_timeout = Some(timeout);
        self
    }
    /// `health_check` URL (default: `url` host, port 13133, path `/`).
    pub fn health_url(mut self, url: impl Into<String>) -> Self {
        self.health_url = Some(url.into());
        self
    }
    /// `zpages` base URL (default: `url` host, port 55679).
    pub fn zpages_url(mut self, url: impl Into<String>) -> Self {
        self.zpages_url = Some(url.into());
        self
    }
    /// The collector's internal telemetry endpoint (`http://collector:8888/metrics`).
    pub fn telemetry_url(mut self, url: impl Into<String>) -> Self {
        self.telemetry_url = Some(url.into());
        self
    }
    /// `service.name` resource attribute.
    pub fn service_name(mut self, name: impl Into<String>) -> Self {
        self.service_name = Some(name.into());
        self
    }
    /// `service.version` resource attribute.
    pub fn service_version(mut self, version: impl Into<String>) -> Self {
        self.service_version = Some(version.into());
        self
    }
    /// `deployment.environment.name` (and `deployment.environment`) resource attribute.
    pub fn environment(mut self, environment: impl Into<String>) -> Self {
        self.environment = Some(environment.into());
        self
    }
    /// Adds a resource attribute (explicit keys win over `OTEL_RESOURCE_ATTRIBUTES`).
    pub fn resource_attribute(
        mut self,
        key: impl Into<String>,
        value: impl Into<AttrValue>,
    ) -> Self {
        self.resource_attributes.insert(key.into(), value.into());
        self
    }
    /// Resource detectors (default all; an empty list disables detection).
    pub fn resource_detectors(mut self, detectors: Vec<ResourceDetector>) -> Self {
        self.resource_detectors = Some(detectors);
        self
    }
    /// Turns one signal on or off in `init_telemetry`.
    pub fn signal(mut self, signal: Signal, enabled: bool) -> Self {
        match signal {
            Signal::Traces => self.traces = Some(enabled),
            Signal::Logs => self.logs = Some(enabled),
            Signal::Metrics => self.metrics = Some(enabled),
        }
        self
    }
    /// Head sampler and ratio.
    pub fn sampler(mut self, sampler: SamplerKind, ratio: f64) -> Self {
        self.sampler = Some(sampler);
        self.sampler_ratio = Some(ratio);
        self
    }
    /// Propagators (default `tracecontext`, `baggage`).
    pub fn propagators(mut self, propagators: Vec<PropagatorKind>) -> Self {
        self.propagators = Some(propagators);
        self
    }
    /// Span batch processor settings.
    pub fn trace_batch(mut self, batch: TraceBatchConfig) -> Self {
        self.trace_batch = Some(batch);
        self
    }
    /// Log batch settings of log clients created from this config.
    pub fn log_batch(mut self, batch: BatchOptions) -> Self {
        self.log_batch = Some(batch);
        self
    }
    /// Interval of the periodic metric reader.
    pub fn metric_export_interval(mut self, interval: Duration) -> Self {
        self.metric_export_interval = Some(interval);
        self
    }
    /// Registers runtime metrics in `init_telemetry`.
    pub fn runtime_metrics(mut self, on: bool) -> Self {
        self.runtime_metrics = Some(on);
        self
    }
    /// Install the providers and the propagator globally (default `true`).
    pub fn set_global(mut self, on: bool) -> Self {
        self.set_global = Some(on);
        self
    }
    /// Connect timeout (default 10 s).
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.common = self.common.connect_timeout(timeout);
        self
    }
    /// Per-attempt timeout of health, ping and version calls (default 30 s).
    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.common = self.common.request_timeout(timeout);
        self
    }
    /// Whole-call timeout including retries (default 120 s).
    pub fn total_timeout(mut self, timeout: Duration) -> Self {
        self.common = self.common.total_timeout(timeout);
        self
    }
    /// Retry policy of ping and version calls (exports use the OTLP exporters' rules).
    pub fn retry(mut self, retry: RetryPolicy) -> Self {
        self.common = self.common.retry(retry);
        self
    }
    /// Maximum retries (default 3).
    pub fn max_retries(mut self, max_retries: u32) -> Self {
        self.common = self.common.max_retries(max_retries);
        self
    }
    /// TLS settings (also used by the exporters).
    pub fn tls(mut self, tls: TlsConfig) -> Self {
        self.common = self.common.tls(tls);
        self
    }
    /// Extra trusted CA certificates (PEM file).
    pub fn ca_cert_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.common = self.common.ca_cert_path(path);
        self
    }
    /// Client certificate and key (PEM files) for mTLS.
    pub fn client_cert(mut self, cert: impl Into<PathBuf>, key: impl Into<PathBuf>) -> Self {
        self.common = self.common.client_cert(cert, key);
        self
    }
    /// Disables TLS verification (tests only; not supported by OTLP gRPC exporters).
    pub fn insecure_skip_verify(mut self, skip: bool) -> Self {
        self.common = self.common.insecure_skip_verify(skip);
        self
    }
    /// Proxy URL (environment proxies are ignored).
    pub fn proxy(mut self, proxy: impl Into<String>) -> Self {
        self.common = self.common.proxy(proxy);
        self
    }
    /// Header sent with every export and with `ping()` (credentials are redacted everywhere).
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.common = self.common.header(name, value);
        self
    }
    /// Text appended to the User-Agent.
    pub fn user_agent_suffix(mut self, suffix: impl Into<String>) -> Self {
        self.common = self.common.user_agent_suffix(suffix);
        self
    }
    /// Request hooks (health, ping and version calls).
    pub fn hooks(mut self, hooks: Arc<dyn Hooks>) -> Self {
        self.common = self.common.hooks(hooks);
        self
    }
    /// Edits any other common setting.
    pub fn common(mut self, f: impl FnOnce(CommonConfigBuilder) -> CommonConfigBuilder) -> Self {
        self.common = f(self.common);
        self
    }

    /// Builds without reading the environment.
    pub fn build(self) -> Result<OtelCollectorConfig> {
        let probe = self.common.clone().max_retries(0).build(DEFAULT_URL)?;
        let common = self.common.build(DEFAULT_URL)?;
        let protocol = self.protocol.unwrap_or(DEFAULT_PROTOCOL);
        if protocol == Protocol::Native {
            return Err(native_error());
        }
        let grpc_url = parse_url(
            "grpcUrl",
            self.grpc_url.as_deref().unwrap_or(DEFAULT_GRPC_URL),
        )?;
        let optional = |name: &str, v: Option<String>| -> Result<Option<Url>> {
            v.filter(|s| !s.is_empty())
                .map(|s| parse_url(name, &s))
                .transpose()
        };
        let traces_url = optional("tracesUrl", self.traces_url)?;
        let logs_url = optional("logsUrl", self.logs_url)?;
        let metrics_url = optional("metricsUrl", self.metrics_url)?;
        let health_url = match self.health_url {
            Some(u) => parse_url("healthUrl", &u)?,
            None => derived_url(common.url(), DEFAULT_HEALTH_PORT)?,
        };
        let zpages_url = match self.zpages_url {
            Some(u) => parse_url("zpagesUrl", &u)?,
            None => derived_url(common.url(), DEFAULT_ZPAGES_PORT)?,
        };
        let telemetry_url = optional("telemetryUrl", self.telemetry_url)?;
        let export_timeout = self.export_timeout.unwrap_or(DEFAULT_EXPORT_TIMEOUT);
        let trace_batch = self.trace_batch.unwrap_or(TraceBatchConfig {
            export_timeout,
            ..TraceBatchConfig::default()
        });
        let service_name = self
            .service_name
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(default_service_name);
        let config = OtelCollectorConfig {
            probe,
            common,
            grpc_url,
            protocol,
            traces_url,
            logs_url,
            metrics_url,
            traces_headers: self.traces_headers,
            logs_headers: self.logs_headers,
            metrics_headers: self.metrics_headers,
            compression: self.compression.unwrap_or_default(),
            export_timeout,
            health_url,
            zpages_url,
            telemetry_url,
            service_name,
            service_version: self.service_version.filter(|s| !s.is_empty()),
            environment: self.environment.filter(|s| !s.is_empty()),
            resource_attributes: self.resource_attributes,
            resource_detectors: self
                .resource_detectors
                .unwrap_or_else(|| ResourceDetector::ALL.to_vec()),
            traces: self.traces.unwrap_or(true),
            logs: self.logs.unwrap_or(true),
            metrics: self.metrics.unwrap_or(true),
            sampler: self.sampler.unwrap_or_default(),
            sampler_ratio: self.sampler_ratio.unwrap_or(1.0),
            propagators: self
                .propagators
                .unwrap_or_else(|| vec![PropagatorKind::TraceContext, PropagatorKind::Baggage]),
            trace_batch,
            log_batch: self.log_batch.unwrap_or_default(),
            metric_export_interval: self
                .metric_export_interval
                .unwrap_or(DEFAULT_METRIC_EXPORT_INTERVAL),
            runtime_metrics: self.runtime_metrics.unwrap_or(false),
            set_global: self.set_global.unwrap_or(true),
        };
        config.validate()?;
        Ok(config)
    }

    /// Fills unset values from the process environment, then builds.
    pub fn build_from_env(self) -> Result<OtelCollectorConfig> {
        self.build_with_env(&EnvReader::new())
    }

    /// Fills unset values from `env`, then builds.
    pub fn build_with_env(mut self, env: &EnvReader) -> Result<OtelCollectorConfig> {
        let e = Env(env);
        if self.protocol.is_none() {
            if let Some((var, v)) = e.lookup("PROTOCOL", &["OTEL_EXPORTER_OTLP_PROTOCOL"]) {
                self.protocol = Some(match v.trim().to_ascii_lowercase().as_str() {
                    "grpc" => Protocol::Grpc,
                    "http/protobuf" => Protocol::HttpProtobuf,
                    _ => {
                        return Err(Error::config(format!(
                            "{var} must be grpc or http/protobuf"
                        )))
                    }
                });
            }
        }
        let grpc_mode = self.protocol == Some(Protocol::Grpc);
        self.apply_common_env(&e, grpc_mode)?;
        self.apply_otlp_env(&e, grpc_mode)?;
        self.apply_resource_env(&e)?;
        self.apply_signal_env(&e)?;
        self.apply_tracing_env(&e)?;
        self.apply_batch_env(&e)?;
        self.build()
    }

    fn apply_common_env(&mut self, e: &Env<'_>, grpc_mode: bool) -> Result<()> {
        // A probe build shows which common values are already set explicitly.
        if let Ok(peek) = self.common.clone().build(DEFAULT_URL) {
            let mut tls = peek.tls().clone();
            let mut changed = false;
            let path = |name: &str, vendor: &[&str]| e.string(name, vendor).map(PathBuf::from);
            if tls.ca_cert_path.is_none() {
                tls.ca_cert_path = path("CA_CERT", &["OTEL_EXPORTER_OTLP_CERTIFICATE"]);
                changed |= tls.ca_cert_path.is_some();
            }
            if tls.client_cert_path.is_none() {
                tls.client_cert_path =
                    path("CLIENT_CERT", &["OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE"]);
                changed |= tls.client_cert_path.is_some();
            }
            if tls.client_key_path.is_none() {
                tls.client_key_path = path("CLIENT_KEY", &["OTEL_EXPORTER_OTLP_CLIENT_KEY"]);
                changed |= tls.client_key_path.is_some();
            }
            if changed {
                self.common = std::mem::take(&mut self.common).tls(tls);
            }
            if !peek.tls().insecure_skip_verify {
                if let Some(skip) = e.bool("INSECURE_SKIP_VERIFY", &[])? {
                    self.common = std::mem::take(&mut self.common).insecure_skip_verify(skip);
                }
            }
            if peek.proxy().is_none() {
                if let Some(p) = e.string("PROXY", &[]) {
                    self.common = std::mem::take(&mut self.common).proxy(p);
                }
            }
            if peek.headers().is_empty() {
                let found = e
                    .lookup("HEADERS", &[])
                    .or_else(|| e.otel("OTEL_EXPORTER_OTLP_HEADERS"));
                if let Some((var, v)) = found {
                    for (k, val) in parse_key_values(&v, &var)? {
                        self.common = std::mem::take(&mut self.common).header(k, val);
                    }
                }
            }
        }
        let mut url_vars = vec![format!("{ALIAS_ENV_PREFIX}URL")];
        if !grpc_mode {
            url_vars.push("OTEL_EXPORTER_OTLP_ENDPOINT".into());
        }
        let refs: Vec<&str> = url_vars.iter().map(String::as_str).collect();
        self.common = std::mem::take(&mut self.common).apply_env(e.0, &refs)?;
        Ok(())
    }

    fn apply_otlp_env(&mut self, e: &Env<'_>, grpc_mode: bool) -> Result<()> {
        if self.grpc_url.is_none() {
            let vendor: &[&str] = if grpc_mode {
                &["OTEL_EXPORTER_OTLP_ENDPOINT"]
            } else {
                &[]
            };
            self.grpc_url = e.string("GRPC_URL", vendor);
        }
        for signal in Signal::ALL {
            let name = signal.env_name();
            let slot = match signal {
                Signal::Traces => &mut self.traces_url,
                Signal::Logs => &mut self.logs_url,
                Signal::Metrics => &mut self.metrics_url,
            };
            if slot.is_none() {
                let vendor = format!("OTEL_EXPORTER_OTLP_{name}_ENDPOINT");
                *slot = e.string(&format!("{name}_URL"), &[&vendor]);
            }
            let headers = match signal {
                Signal::Traces => &mut self.traces_headers,
                Signal::Logs => &mut self.logs_headers,
                Signal::Metrics => &mut self.metrics_headers,
            };
            if headers.is_empty() {
                if let Some((var, v)) = e.otel(&format!("OTEL_EXPORTER_OTLP_{name}_HEADERS")) {
                    *headers = parse_key_values(&v, &var)?.into_iter().collect();
                }
            }
        }
        if self.compression.is_none() {
            if let Some((var, v)) = e.lookup("COMPRESSION", &["OTEL_EXPORTER_OTLP_COMPRESSION"]) {
                self.compression = Some(match v.trim().to_ascii_lowercase().as_str() {
                    "gzip" => Compression::Gzip,
                    "none" => Compression::None,
                    _ => return Err(Error::config(format!("{var} must be gzip or none"))),
                });
            }
        }
        if self.export_timeout.is_none() {
            self.export_timeout = e.duration("EXPORT_TIMEOUT", &["OTEL_EXPORTER_OTLP_TIMEOUT"])?;
        }
        if self.health_url.is_none() {
            self.health_url = e.string("HEALTH_URL", &[]);
        }
        if self.zpages_url.is_none() {
            self.zpages_url = e.string("ZPAGES_URL", &[]);
        }
        if self.telemetry_url.is_none() {
            self.telemetry_url = e.string("TELEMETRY_URL", &[]);
        }
        Ok(())
    }

    fn apply_resource_env(&mut self, e: &Env<'_>) -> Result<()> {
        let mut env_attrs = Vec::new();
        if let Some((var, v)) = e.lookup("RESOURCE_ATTRIBUTES", &["OTEL_RESOURCE_ATTRIBUTES"]) {
            env_attrs = parse_key_values(&v, &var)?;
        }
        if self.service_name.is_none() {
            self.service_name = e.string("SERVICE_NAME", &["OTEL_SERVICE_NAME"]);
        }
        if self.service_version.is_none() {
            self.service_version = e.string("SERVICE_VERSION", &[]);
        }
        if self.environment.is_none() {
            self.environment = e.string("ENVIRONMENT", &[]);
        }
        if self.resource_detectors.is_none() {
            if let Some((var, v)) = e.lookup("RESOURCE_DETECTORS", &[]) {
                self.resource_detectors = Some(ResourceDetector::parse_list(&v).map_err(|_| {
                    Error::config(format!(
                        "{var} must list host, process, container, k8s or none"
                    ))
                })?);
            }
        }
        if self.service_name.is_none() && !self.resource_attributes.contains_key("service.name") {
            self.service_name = env_attrs
                .iter()
                .find(|(k, _)| k == "service.name")
                .map(|(_, v)| v.clone())
                .filter(|v| !v.is_empty());
        }
        for (k, v) in env_attrs {
            self.resource_attributes
                .entry(k)
                .or_insert(AttrValue::String(v));
        }
        Ok(())
    }

    fn apply_signal_env(&mut self, e: &Env<'_>) -> Result<()> {
        let sdk_disabled = e
            .otel("OTEL_SDK_DISABLED")
            .map(|(var, v)| {
                parse_bool(&v).ok_or_else(|| {
                    Error::config(format!("environment variable {var} is not a boolean"))
                })
            })
            .transpose()?
            .unwrap_or(false);
        for signal in Signal::ALL {
            let name = signal.env_name();
            let slot = match signal {
                Signal::Traces => &mut self.traces,
                Signal::Logs => &mut self.logs,
                Signal::Metrics => &mut self.metrics,
            };
            if slot.is_some() {
                continue;
            }
            if sdk_disabled {
                *slot = Some(false);
                continue;
            }
            if let Some(on) = e.bool(name, &[])? {
                *slot = Some(on);
                continue;
            }
            if let Some((_, v)) = e.otel(&format!("OTEL_{name}_EXPORTER")) {
                *slot = Some(!v.trim().eq_ignore_ascii_case("none"));
            }
        }
        Ok(())
    }

    fn apply_tracing_env(&mut self, e: &Env<'_>) -> Result<()> {
        if self.sampler.is_none() {
            if let Some((var, v)) = e.lookup("SAMPLER", &["OTEL_TRACES_SAMPLER"]) {
                self.sampler = Some(
                    v.parse()
                        .map_err(|_| Error::config(format!("{var} names an unknown sampler")))?,
                );
            }
        }
        if self.sampler_ratio.is_none() {
            if let Some((var, v)) = e.lookup("SAMPLER_RATIO", &["OTEL_TRACES_SAMPLER_ARG"]) {
                let ratio = v
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|r| (0.0..=1.0).contains(r))
                    .ok_or_else(|| {
                        Error::config(format!("{var} must be a number between 0 and 1"))
                    })?;
                self.sampler_ratio = Some(ratio);
            }
        }
        if self.propagators.is_none() {
            if let Some((_, v)) = e.lookup("PROPAGATORS", &["OTEL_PROPAGATORS"]) {
                let list = v
                    .split(',')
                    .map(|p| p.trim().to_ascii_lowercase())
                    .filter(|p| !p.is_empty() && p != "none")
                    .map(|p| p.parse())
                    .collect::<Result<Vec<PropagatorKind>>>()?;
                self.propagators = Some(list);
            }
        }
        if self.runtime_metrics.is_none() {
            self.runtime_metrics = e.bool("RUNTIME_METRICS", &[])?;
        }
        Ok(())
    }

    fn apply_batch_env(&mut self, e: &Env<'_>) -> Result<()> {
        if self.trace_batch.is_none() {
            let export_timeout = self.export_timeout.unwrap_or(DEFAULT_EXPORT_TIMEOUT);
            let mut b = TraceBatchConfig {
                export_timeout,
                ..TraceBatchConfig::default()
            };
            let mut changed = false;
            if let Some(d) = e.otel_duration("OTEL_BSP_SCHEDULE_DELAY")? {
                b.scheduled_delay = d;
                changed = true;
            }
            if let Some(d) = e.otel_duration("OTEL_BSP_EXPORT_TIMEOUT")? {
                b.export_timeout = d;
                changed = true;
            }
            if let Some(n) = e.otel_usize("OTEL_BSP_MAX_QUEUE_SIZE")? {
                b.max_queue_size = n;
                changed = true;
            }
            if let Some(n) = e.otel_usize("OTEL_BSP_MAX_EXPORT_BATCH_SIZE")? {
                b.max_export_batch_size = n;
                changed = true;
            }
            if changed {
                self.trace_batch = Some(b);
            }
        }
        if self.log_batch.is_none() {
            let mut b = BatchOptions::default();
            let mut changed = false;
            if let Some(d) = e.otel_duration("OTEL_BLRP_SCHEDULE_DELAY")? {
                b.flush_interval = d;
                changed = true;
            }
            if let Some(d) = e.otel_duration("OTEL_BLRP_EXPORT_TIMEOUT")? {
                b.export_timeout = d;
                changed = true;
            }
            if let Some(n) = e.otel_usize("OTEL_BLRP_MAX_QUEUE_SIZE")? {
                b.max_queue_size = n;
                changed = true;
            }
            if let Some(n) = e.otel_usize("OTEL_BLRP_MAX_EXPORT_BATCH_SIZE")? {
                b.max_batch_size = n;
                changed = true;
            }
            if changed {
                self.log_batch = Some(b);
            }
        }
        if self.metric_export_interval.is_none() {
            self.metric_export_interval =
                e.duration("METRIC_EXPORT_INTERVAL", &["OTEL_METRIC_EXPORT_INTERVAL"])?;
        }
        Ok(())
    }
}

/// Environment lookups with the `GROW_OTEL_*` alias inserted after the SDK prefix.
struct Env<'a>(&'a EnvReader);

impl Env<'_> {
    fn names(name: &str, vendor: &[&str]) -> Vec<String> {
        std::iter::once(format!("{ALIAS_ENV_PREFIX}{name}"))
            .chain(vendor.iter().map(|v| (*v).to_owned()))
            .collect()
    }

    fn lookup(&self, name: &str, vendor: &[&str]) -> Option<(String, String)> {
        let names = Self::names(name, vendor);
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        self.0.lookup(name, &refs)
    }

    fn string(&self, name: &str, vendor: &[&str]) -> Option<String> {
        self.lookup(name, vendor).map(|(_, v)| v)
    }

    fn duration(&self, name: &str, vendor: &[&str]) -> Result<Option<Duration>> {
        let names = Self::names(name, vendor);
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        self.0.duration(name, &refs)
    }

    fn bool(&self, name: &str, vendor: &[&str]) -> Result<Option<bool>> {
        let names = Self::names(name, vendor);
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        self.0.bool(name, &refs)
    }

    /// A standard variable only (the SDK prefix alone is never set).
    fn otel(&self, var: &str) -> Option<(String, String)> {
        self.0.lookup("", &[var])
    }

    fn otel_duration(&self, var: &str) -> Result<Option<Duration>> {
        self.0.duration("", &[var])
    }

    fn otel_usize(&self, var: &str) -> Result<Option<usize>> {
        match self.0.int("", &[var])? {
            None => Ok(None),
            Some(n) => usize::try_from(n)
                .map(Some)
                .map_err(|_| Error::config(format!("environment variable {var} must be >= 0"))),
        }
    }
}

/// Parses `k=v,k2=v2` (the OpenTelemetry list format; keys and values are URL-decoded and
/// trimmed). `CONFIG` naming `source` (never the value) when malformed.
pub fn parse_key_values(raw: &str, source: &str) -> Result<Vec<(String, String)>> {
    let bad = || Error::config(format!("{source} must be a list of key=value pairs"));
    let mut out = Vec::new();
    for part in raw.split(',').filter(|p| !p.trim().is_empty()) {
        let (k, v) = part.split_once('=').ok_or_else(bad)?;
        let key = percent_decode(k.trim()).ok_or_else(bad)?;
        let value = percent_decode(v.trim()).ok_or_else(bad)?;
        if key.is_empty() {
            return Err(bad());
        }
        out.push((key, value));
    }
    Ok(out)
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn parse_url(name: &str, raw: &str) -> Result<Url> {
    Url::parse(raw)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https") && u.has_host())
        .ok_or_else(|| Error::config(format!("{name} must be an absolute http or https URL")))
}

/// `base`'s scheme and host with another port and the root path.
fn derived_url(base: &Url, port: u16) -> Result<Url> {
    let mut u = base.clone();
    u.set_path("/");
    u.set_query(None);
    u.set_fragment(None);
    let _ = u.set_username("");
    let _ = u.set_password(None);
    u.set_port(Some(port))
        .map_err(|_| Error::config("url has no host to derive the admin endpoints from"))?;
    Ok(u)
}

/// The OpenTelemetry fallback service name: `unknown_service:<executable name>`.
pub fn default_service_name() -> String {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .filter(|n| !n.is_empty());
    format!("unknown_service:{}", exe.as_deref().unwrap_or("rust"))
}
