// Service code (OpenTelemetry Collector): metric helpers and the OTLP metric pipeline.
//! Metrics: `meter(name)`, `counter`, `up_down_counter`, `histogram`, `gauge` helpers (on the
//! global meter provider or a [`TelemetryHandle`](crate::TelemetryHandle)), the official OTLP
//! metric exporter, a meter provider with a periodic reader, and runtime metrics.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter, UpDownCounter};

use opentelemetry::{InstrumentationScope, KeyValue};
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig, WithTonicConfig};
use opentelemetry_sdk::metrics::periodic_reader_with_async_runtime::PeriodicReader;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::runtime::Tokio;
use opentelemetry_sdk::Resource;

use crate::config::{OtelCollectorConfig, Signal};
use crate::core::transport::SDK_VERSION;
use crate::core::{Error, Result};
use crate::logs::INSTRUMENTATION_SCOPE;
use crate::otlp::{grpc_metadata, reqwest_client, tonic_tls, ClientSettings};
use crate::tracing::{Compression, Protocol};

/// Meter of the instrument helpers when [`InstrumentOptions::meter`] is unset.
pub const DEFAULT_METER_NAME: &str = INSTRUMENTATION_SCOPE;
/// Meter of the runtime metrics.
pub const RUNTIME_METER_NAME: &str = "grow-codes-opentelemetry-collector/runtime";

/// Describes a metric instrument. The default value is valid.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InstrumentOptions {
    /// Instrument description.
    pub description: Option<String>,
    /// UCUM unit (`ms`, `By`, `{request}`).
    pub unit: Option<String>,
    /// Meter to create the instrument on (default [`DEFAULT_METER_NAME`]).
    pub meter: Option<String>,
    /// Explicit histogram bucket boundaries, strictly ascending (histograms only; `None` =
    /// the SDK default).
    pub boundaries: Option<Vec<f64>>,
}

impl InstrumentOptions {
    /// Sets the description.
    pub fn description(mut self, d: impl Into<String>) -> Self {
        self.description = Some(d.into());
        self
    }
    /// Sets the unit.
    pub fn unit(mut self, u: impl Into<String>) -> Self {
        self.unit = Some(u.into());
        self
    }
    /// Sets the meter name.
    pub fn meter(mut self, m: impl Into<String>) -> Self {
        self.meter = Some(m.into());
        self
    }
    /// Sets histogram bucket boundaries.
    pub fn boundaries(mut self, b: Vec<f64>) -> Self {
        self.boundaries = Some(b);
        self
    }
}

/// A named meter of a provider (empty name = [`DEFAULT_METER_NAME`]).
pub(crate) fn meter_of(provider: &dyn Fn(InstrumentationScope) -> Meter, name: &str) -> Meter {
    let name = if name.is_empty() {
        DEFAULT_METER_NAME
    } else {
        name
    };
    provider(
        InstrumentationScope::builder(name.to_owned())
            .with_version(SDK_VERSION)
            .build(),
    )
}

fn validate_name(op: &str, name: &str) -> Result<()> {
    let mut chars = name.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphabetic());
    let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '/'));
    if !first_ok || !rest_ok || name.len() > 255 {
        return Err(Error::validation(format!(
            "{op}: invalid instrument name {name:?} (letter first, then letters, digits, _ . - /; at most 255)"
        )));
    }
    Ok(())
}

/// Instrument factory over one meter provider.
pub(crate) struct Instruments<'a>(pub &'a dyn Fn(InstrumentationScope) -> Meter);

impl Instruments<'_> {
    fn meter(&self, o: &InstrumentOptions) -> Meter {
        meter_of(self.0, o.meter.as_deref().unwrap_or(""))
    }

    pub(crate) fn counter(&self, name: &str, o: &InstrumentOptions) -> Result<Counter<u64>> {
        validate_name("metrics.counter", name)?;
        let meter = self.meter(o);
        let mut b = meter.u64_counter(name.to_owned());
        if let Some(d) = &o.description {
            b = b.with_description(d.clone());
        }
        if let Some(u) = &o.unit {
            b = b.with_unit(u.clone());
        }
        Ok(b.build())
    }

    pub(crate) fn up_down_counter(
        &self,
        name: &str,
        o: &InstrumentOptions,
    ) -> Result<UpDownCounter<i64>> {
        validate_name("metrics.upDownCounter", name)?;
        let meter = self.meter(o);
        let mut b = meter.i64_up_down_counter(name.to_owned());
        if let Some(d) = &o.description {
            b = b.with_description(d.clone());
        }
        if let Some(u) = &o.unit {
            b = b.with_unit(u.clone());
        }
        Ok(b.build())
    }

    pub(crate) fn histogram(&self, name: &str, o: &InstrumentOptions) -> Result<Histogram<f64>> {
        validate_name("metrics.histogram", name)?;
        let meter = self.meter(o);
        let mut b = meter.f64_histogram(name.to_owned());
        if let Some(d) = &o.description {
            b = b.with_description(d.clone());
        }
        if let Some(u) = &o.unit {
            b = b.with_unit(u.clone());
        }
        if let Some(bounds) = &o.boundaries {
            let ascending = bounds.windows(2).all(|w| w[0] < w[1]);
            if !ascending || bounds.iter().any(|v| !v.is_finite()) {
                return Err(Error::validation(
                    "metrics.histogram: boundaries must be finite and strictly ascending",
                ));
            }
            b = b.with_boundaries(bounds.clone());
        }
        Ok(b.build())
    }

    pub(crate) fn gauge(&self, name: &str, o: &InstrumentOptions) -> Result<Gauge<f64>> {
        validate_name("metrics.gauge", name)?;
        let meter = self.meter(o);
        let mut b = meter.f64_gauge(name.to_owned());
        if let Some(d) = &o.description {
            b = b.with_description(d.clone());
        }
        if let Some(u) = &o.unit {
            b = b.with_unit(u.clone());
        }
        Ok(b.build())
    }
}

fn global_meter(scope: InstrumentationScope) -> Meter {
    opentelemetry::global::meter_provider().meter_with_scope(scope)
}

/// A named meter of the global meter provider (set by `init_telemetry` unless `set_global` is
/// off); an empty name means [`DEFAULT_METER_NAME`].
pub fn meter(name: &str) -> Meter {
    meter_of(&global_meter, name)
}

/// A monotonic `u64` counter (OTLP sum, cumulative) on the global meter provider. `VALIDATION`
/// for an invalid instrument name.
pub fn counter(name: &str, opts: InstrumentOptions) -> Result<Counter<u64>> {
    Instruments(&global_meter).counter(name, &opts)
}

/// A non-monotonic `i64` counter on the global meter provider.
pub fn up_down_counter(name: &str, opts: InstrumentOptions) -> Result<UpDownCounter<i64>> {
    Instruments(&global_meter).up_down_counter(name, &opts)
}

/// An `f64` histogram (with `opts.boundaries` as explicit buckets) on the global meter provider.
pub fn histogram(name: &str, opts: InstrumentOptions) -> Result<Histogram<f64>> {
    Instruments(&global_meter).histogram(name, &opts)
}

/// A synchronous `f64` gauge (last value) on the global meter provider.
pub fn gauge(name: &str, opts: InstrumentOptions) -> Result<Gauge<f64>> {
    Instruments(&global_meter).gauge(name, &opts)
}

/// The official OTLP metric exporter for a config: `http/protobuf` posts protobuf to the
/// metrics endpoint, `grpc` uses `MetricsService/Export`. Headers, compression, export timeout
/// and TLS come from the config; temporality is cumulative.
pub fn metric_exporter(config: &OtelCollectorConfig) -> Result<opentelemetry_otlp::MetricExporter> {
    let endpoint = config.signal_endpoint(Signal::Metrics);
    let headers = config.signal_headers(Signal::Metrics);
    let gzip = config.compression() == Compression::Gzip;
    let built = match config.protocol() {
        Protocol::Grpc => {
            let mut b = opentelemetry_otlp::MetricExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint.clone())
                .with_timeout(config.export_timeout())
                .with_metadata(grpc_metadata(&headers)?);
            if gzip {
                b = b.with_compression(opentelemetry_otlp::Compression::Gzip);
            }
            if let Some(tls) = tonic_tls(&endpoint, config.common().tls())? {
                b = b.with_tls_config(tls);
            }
            b.build()
        }
        _ => {
            let c = config.common();
            let client = reqwest_client(&ClientSettings {
                tls: c.tls().clone(),
                proxy: c.proxy().map(str::to_owned),
                connect_timeout: Some(c.connect_timeout()),
                timeout: config.export_timeout(),
            })?;
            let mut b = opentelemetry_otlp::MetricExporter::builder()
                .with_http()
                .with_endpoint(endpoint)
                .with_timeout(config.export_timeout())
                .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
                .with_headers(headers.into_iter().collect::<HashMap<_, _>>())
                .with_http_client(client);
            if gzip {
                b = b.with_compression(opentelemetry_otlp::Compression::Gzip);
            }
            b.build()
        }
    };
    built.map_err(|e| Error::config(format!("metrics: cannot build the OTLP exporter: {e}")))
}

/// A meter provider with `resource`, the OTLP exporter of `config` and a periodic reader on the
/// tokio runtime (`interval`, default the config's `metric_export_interval`). Must be called
/// inside a tokio runtime.
pub fn create_meter_provider(
    config: &OtelCollectorConfig,
    resource: Resource,
    interval: Option<Duration>,
) -> Result<SdkMeterProvider> {
    let interval = interval.unwrap_or_else(|| config.metric_export_interval());
    if interval.is_zero() {
        return Err(Error::config("metrics: export interval must be positive"));
    }
    tokio::runtime::Handle::try_current()
        .map_err(|_| Error::config("metrics must be started inside a tokio runtime"))?;
    let exporter = metric_exporter(config)?;
    let reader = PeriodicReader::builder(exporter, Tokio)
        .with_interval(interval)
        .build();
    Ok(SdkMeterProvider::builder()
        .with_resource(resource)
        .with_reader(reader)
        .build())
}

/// Stops the runtime metrics callbacks.
#[derive(Debug, Clone)]
pub struct RuntimeMetricsHandle {
    stopped: Arc<AtomicBool>,
}

impl RuntimeMetricsHandle {
    /// Stops reporting (idempotent).
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }

    /// Whether [`stop`](Self::stop) was called.
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }
}

/// Reads a `/proc/self/*` file (Linux); `None` elsewhere.
fn proc_file(name: &str) -> Option<String> {
    std::fs::read_to_string(format!("/proc/self/{name}")).ok()
}

/// Resident set size in bytes from `/proc/self/statm` (4 KiB pages).
pub(crate) fn rss_bytes(statm: &str) -> Option<u64> {
    statm
        .split_whitespace()
        .nth(1)?
        .parse::<u64>()
        .ok()
        .map(|p| p * 4096)
}

/// User and system CPU seconds from `/proc/self/stat` (100 ticks per second).
pub(crate) fn cpu_seconds(stat: &str) -> Option<(f64, f64)> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let user = fields.get(11)?.parse::<f64>().ok()?;
    let system = fields.get(12)?.parse::<f64>().ok()?;
    Some((user / 100.0, system / 100.0))
}

/// Thread count from `/proc/self/status`.
pub(crate) fn thread_count(status: &str) -> Option<u64> {
    status
        .lines()
        .find_map(|l| l.strip_prefix("Threads:"))
        .and_then(|v| v.trim().parse().ok())
}

/// Registers Rust runtime metrics on `meter` (observable instruments, read at every
/// collection): `process.memory.usage` (RSS, By; Linux), `process.cpu.time` (s, `cpu.mode`
/// user/system; Linux), `process.thread.count` (Linux), `process.uptime` (s since
/// registration) and, inside a tokio runtime, `rust.tokio.workers` and `rust.tokio.alive_tasks`.
pub fn register_runtime_metrics(meter: &Meter) -> RuntimeMetricsHandle {
    let stopped = Arc::new(AtomicBool::new(false));
    let live = {
        let s = Arc::clone(&stopped);
        move || !s.load(Ordering::SeqCst)
    };
    let l = live.clone();
    meter
        .u64_observable_gauge("process.memory.usage")
        .with_unit("By")
        .with_description("Resident set size of the process.")
        .with_callback(move |o| {
            if let Some(v) = l()
                .then(|| proc_file("statm"))
                .flatten()
                .as_deref()
                .and_then(rss_bytes)
            {
                o.observe(v, &[]);
            }
        })
        .build();
    let l = live.clone();
    meter
        .f64_observable_counter("process.cpu.time")
        .with_unit("s")
        .with_description("CPU time used by the process.")
        .with_callback(move |o| {
            if let Some((user, system)) = l()
                .then(|| proc_file("stat"))
                .flatten()
                .as_deref()
                .and_then(cpu_seconds)
            {
                o.observe(user, &[KeyValue::new("cpu.mode", "user")]);
                o.observe(system, &[KeyValue::new("cpu.mode", "system")]);
            }
        })
        .build();
    let l = live.clone();
    meter
        .u64_observable_gauge("process.thread.count")
        .with_description("Threads of the process.")
        .with_callback(move |o| {
            if let Some(v) = l()
                .then(|| proc_file("status"))
                .flatten()
                .as_deref()
                .and_then(thread_count)
            {
                o.observe(v, &[]);
            }
        })
        .build();
    let l = live.clone();
    let started = Instant::now();
    meter
        .f64_observable_gauge("process.uptime")
        .with_unit("s")
        .with_description("Time since the runtime metrics were registered.")
        .with_callback(move |o| {
            if l() {
                o.observe(started.elapsed().as_secs_f64(), &[]);
            }
        })
        .build();
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let l = live.clone();
        let h = handle.clone();
        meter
            .u64_observable_gauge("rust.tokio.workers")
            .with_description("Worker threads of the tokio runtime.")
            .with_callback(move |o| {
                if l() {
                    o.observe(h.metrics().num_workers() as u64, &[]);
                }
            })
            .build();
        let l = live;
        meter
            .u64_observable_gauge("rust.tokio.alive_tasks")
            .with_description("Alive tasks of the tokio runtime.")
            .with_callback(move |o| {
                if l() {
                    o.observe(handle.metrics().num_alive_tasks() as u64, &[]);
                }
            })
            .build();
    }
    RuntimeMetricsHandle { stopped }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_parsers() {
        assert_eq!(rss_bytes("100 25 3 4 5 6 7"), Some(25 * 4096));
        assert_eq!(rss_bytes("100"), None);
        let stat = "123 (my (odd) proc) S 1 2 3 4 5 6 7 8 9 10 250 50 0 0";
        assert_eq!(cpu_seconds(stat), Some((2.5, 0.5)));
        assert_eq!(cpu_seconds("no paren"), None);
        assert_eq!(cpu_seconds("1 (x) S 1"), None);
        assert_eq!(thread_count("Name:\tx\nThreads:\t 7\n"), Some(7));
        assert_eq!(thread_count("Name: x"), None);
        let _ = proc_file("statm");
    }

    #[test]
    fn instrument_options_builders() {
        let o = InstrumentOptions::default()
            .description("d")
            .unit("ms")
            .meter("m")
            .boundaries(vec![1.0]);
        assert_eq!(o.meter.as_deref(), Some("m"));
        assert_eq!(o.boundaries, Some(vec![1.0]));
    }
}
