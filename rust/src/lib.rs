// Skeleton created by sdk-kit 0.1.0; owned by this repository (edit freely).
//! # grow-opentelemetry-collector
//!
//! Rust SDK for the [OpenTelemetry Collector](https://grow-codes-packages.github.io/opentelemetry-collector/)
//! by Grow Codes: one-call telemetry (traces, logs and metrics over OTLP with the official
//! `opentelemetry-otlp` exporters), collector health, ping and version.
//!
//! ```no_run
//! # async fn demo() -> grow_opentelemetry_collector::Result<()> {
//! use grow_opentelemetry_collector::{init_telemetry, InstrumentOptions, OtelCollectorConfig};
//! use grow_opentelemetry_collector::opentelemetry::trace::Tracer as _;
//!
//! // explicit > GROW_OPENTELEMETRY_COLLECTOR_* > GROW_OTEL_* > OTEL_* > defaults
//! let config = OtelCollectorConfig::builder().service_name("checkout").build_from_env()?;
//! let telemetry = init_telemetry(&config)?;
//! telemetry.tracer("checkout", None).in_span("charge-card", |_cx| {});
//! telemetry.counter("orders", InstrumentOptions::default())?.add(1, &[]);
//! telemetry.logger(Some("http"), Default::default()).info("listening", Default::default());
//! telemetry.shutdown().await?;
//! # Ok(())
//! # }
//! ```
//!
//! Modules: [`OtelCollectorClient`] (health with the zpages fallback, ping, version, log
//! exporter and log client), [`CollectorHealthClient`], [`init_telemetry`] /
//! [`TelemetryHandle`], [`init_tracing`], [`OtlpLogExporter`], the metric helpers
//! ([`counter`], [`histogram`], [`gauge`], …), resource detection, and the [`tracing`] and
//! [`logging`] category helpers. The query clients ([`CollectorTraceQueryClient`],
//! [`CollectorLogQueryClient`]) return `UNSUPPORTED`: the collector stores nothing.
//!
//! Features: `blocking` (a blocking client), `testing` (in-memory fakes and the conformance
//! runner), `tracing-layer` and `log-bridge` (logging adapters, on by default), `tower` and
//! `reqwest-middleware` (tracing instrumentation).
#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod core;

mod client;
mod config;
mod health;
mod logs;
mod metrics;
mod otlp;
mod query;
pub mod resource;
mod setup;
mod telemetry;

#[cfg(feature = "blocking")]
#[cfg_attr(docsrs, doc(cfg(feature = "blocking")))]
pub mod blocking;

#[cfg(feature = "testing")]
#[cfg_attr(docsrs, doc(cfg(feature = "testing")))]
pub mod testing;

pub mod logging;
pub mod tracing;

/// The OpenTelemetry API crate this SDK is built on (same version, for tracers and meters).
pub use opentelemetry;
/// The OpenTelemetry SDK crate this SDK is built on.
pub use opentelemetry_sdk;

pub use crate::core::{Error, ErrorCode, HealthStatus, Result, RetryPolicy, Secret};

pub use crate::client::{parse_servicez_version, parse_target_info_version, OtelCollectorClient};
pub use crate::config::{
    default_service_name, parse_key_values, OtelCollectorConfig, OtelCollectorConfigBuilder,
    Signal, ALIAS_ENV_PREFIX, DEFAULT_EXPORT_TIMEOUT, DEFAULT_GRPC_URL, DEFAULT_HEALTH_PORT,
    DEFAULT_METRIC_EXPORT_INTERVAL, DEFAULT_PROTOCOL, DEFAULT_URL, DEFAULT_ZPAGES_PORT,
};
pub use crate::health::{CollectorHealthClient, DEFAULT_READY_INTERVAL, DEFAULT_READY_TIMEOUT};
pub use crate::logs::{
    create_log_client, LogClientOptions, OtlpLogExporter, INSTRUMENTATION_SCOPE,
};
pub use crate::metrics::{
    counter, create_meter_provider, gauge, histogram, meter, metric_exporter,
    register_runtime_metrics, up_down_counter, InstrumentOptions, RuntimeMetricsHandle,
    DEFAULT_METER_NAME, RUNTIME_METER_NAME,
};
pub use crate::otlp::{
    decode_partial_success, decode_rpc_status, grpc_error_code, grpc_status_error,
    http_status_error, otlp_error_text, PartialSuccess,
};
pub use crate::query::{CollectorLogQueryClient, CollectorTraceQueryClient};
pub use crate::resource::{
    detect_resource, detect_resource_attributes, telemetry_resource_attributes, ResourceDetector,
};
pub use crate::setup::{
    collector_exporter, init_tracing, init_tracing_with_collector, init_tracing_with_env,
    resolve_traces_endpoint, tracing_config_from, tracing_config_from_env,
    DEFAULT_OTLP_HTTP_TRACES_ENDPOINT,
};
pub use crate::telemetry::{
    init_telemetry, init_telemetry_from_env, init_telemetry_with, TelemetryHandle, TelemetryOptions,
};

/// Service-prefixed alias of [`Error`].
pub type OtelCollectorError = Error;
