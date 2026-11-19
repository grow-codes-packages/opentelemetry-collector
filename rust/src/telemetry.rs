// Service code (OpenTelemetry Collector): the one-call telemetry bootstrap.
//! [`init_telemetry`]: traces, logs and metrics with one shared resource, exported over OTLP
//! to the collector that the platform deploys with every Grow stack.

use std::borrow::Cow;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter, MeterProvider as _, UpDownCounter};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::InstrumentationScope;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::{Sampler, SdkTracer, SdkTracerProvider};
use opentelemetry_sdk::Resource;

use crate::config::{OtelCollectorConfig, Signal};
use crate::core::{Error, Result};
use crate::logging::{
    Attributes, BatchOptions, BatchStats, BatchingLogClient, LogClient, LogEntry, Logger, Severity,
};
use crate::logs::{create_log_client, LogClientOptions};
use crate::metrics::{
    create_meter_provider, meter_of, register_runtime_metrics, InstrumentOptions, Instruments,
    RuntimeMetricsHandle, RUNTIME_METER_NAME,
};
use crate::resource::{resource_from_attributes, telemetry_resource_attributes};
use crate::setup::collector_exporter;
use crate::tracing::{init_tracing_with, TracingHandle};

/// Options of [`init_telemetry_with`]; unset fields come from the config.
#[derive(Debug, Clone, Default)]
pub struct TelemetryOptions {
    /// Turn traces on or off (default: the config's signal switch).
    pub traces: Option<bool>,
    /// Turn logs on or off (default: the config's signal switch).
    pub logs: Option<bool>,
    /// Turn metrics on or off (default: the config's signal switch).
    pub metrics: Option<bool>,
    /// Install the tracer provider, propagator and meter provider globally (default: the
    /// config's `set_global`, `true`).
    pub set_global: Option<bool>,
    /// Log batching (default: the config's `log_batch`).
    pub log_batch: Option<BatchOptions>,
    /// Minimum severity of `logger()` loggers (default `Info`).
    pub min_severity: Option<Severity>,
    /// Metric export interval (default: the config's `metric_export_interval`).
    pub metric_export_interval: Option<Duration>,
    /// Runtime metrics (default: the config's `runtime_metrics`).
    pub runtime_metrics: Option<bool>,
}

/// A running telemetry setup (all three signals). Cheap to clone.
#[derive(Clone)]
pub struct TelemetryHandle {
    inner: Arc<Inner>,
}

struct Inner {
    config: OtelCollectorConfig,
    resource_attributes: Attributes,
    resource: Resource,
    tracing: Option<TracingHandle>,
    noop_tracers: SdkTracerProvider,
    meter_provider: Option<SdkMeterProvider>,
    noop_meters: SdkMeterProvider,
    log_client: Option<Arc<BatchingLogClient>>,
    runtime: Option<RuntimeMetricsHandle>,
    shutdown: AtomicBool,
}

impl fmt::Debug for TelemetryHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TelemetryHandle")
            .field("service_name", &self.inner.config.service_name())
            .field("traces", &self.inner.tracing.is_some())
            .field("logs", &self.inner.log_client.is_some())
            .field("metrics", &self.inner.meter_provider.is_some())
            .field("shutdown", &self.is_shutdown())
            .finish()
    }
}

/// [`init_telemetry_with`] with default options.
pub fn init_telemetry(config: &OtelCollectorConfig) -> Result<TelemetryHandle> {
    init_telemetry_with(config, TelemetryOptions::default())
}

/// [`init_telemetry`] with the config read from the process environment.
pub fn init_telemetry_from_env() -> Result<TelemetryHandle> {
    init_telemetry(&OtelCollectorConfig::from_env()?)
}

/// One-call OpenTelemetry bootstrap for every Grow stack (the platform deploys the collector
/// with each stack): traces (`init_tracing` with the official OTLP span exporter), logs
/// ([`OtlpLogExporter`](crate::OtlpLogExporter) + [`BatchingLogClient`]) and metrics (a meter
/// provider with the official OTLP metric exporter and a periodic reader), all with one shared
/// resource (detected host, process, container and Kubernetes attributes < resource attributes
/// < service fields), gRPC or `http/protobuf` per the config. Signals follow the config
/// (`TRACES`/`LOGS`/`METRICS`, `OTEL_SDK_DISABLED`, `OTEL_<SIGNAL>_EXPORTER=none`) unless the
/// options turn them on or off. Exporter failures never surface at the caller. Must be called
/// inside a tokio runtime; call [`TelemetryHandle::shutdown`] before exit.
pub fn init_telemetry_with(
    config: &OtelCollectorConfig,
    opts: TelemetryOptions,
) -> Result<TelemetryHandle> {
    config.validate()?;
    tokio::runtime::Handle::try_current()
        .map_err(|_| Error::config("init_telemetry must be called inside a tokio runtime"))?;
    let set_global = opts.set_global.unwrap_or(config.set_global());
    let resource_attributes = telemetry_resource_attributes(config);
    let resource = resource_from_attributes(&resource_attributes);
    let traces = opts.traces.unwrap_or(config.signal_enabled(Signal::Traces));
    let logs = opts.logs.unwrap_or(config.signal_enabled(Signal::Logs));
    let metrics = opts
        .metrics
        .unwrap_or(config.signal_enabled(Signal::Metrics));

    let mut tracing = None;
    let mut log_client: Option<Arc<BatchingLogClient>> = None;
    let mut meter_provider = None;
    let mut runtime = None;
    let result = (|| -> Result<()> {
        if traces {
            let mut tc = config.tracing_config();
            tc.resource_attributes = resource_attributes.clone();
            tc.set_global = set_global;
            tracing = Some(init_tracing_with(tc, collector_exporter)?);
        }
        if logs {
            log_client = Some(Arc::new(create_log_client(
                config,
                LogClientOptions {
                    batch: opts.log_batch.clone(),
                    resource: Some(resource_attributes.clone()),
                    redaction: None,
                    min_severity: opts.min_severity,
                },
            )?));
        }
        if metrics {
            let mp = create_meter_provider(config, resource.clone(), opts.metric_export_interval)?;
            if set_global {
                opentelemetry::global::set_meter_provider(mp.clone());
            }
            if opts.runtime_metrics.unwrap_or(config.runtime_metrics()) {
                runtime = Some(register_runtime_metrics(&mp.meter_with_scope(
                    InstrumentationScope::builder(RUNTIME_METER_NAME).build(),
                )));
            }
            meter_provider = Some(mp);
        }
        Ok(())
    })();
    if let Err(e) = result {
        if let Some(t) = tracing {
            tokio::spawn(async move {
                let _ = t.shutdown().await;
            });
        }
        if let Some(lc) = log_client {
            tokio::spawn(async move {
                let _ = lc.close().await;
            });
        }
        return Err(e);
    }
    Ok(TelemetryHandle {
        inner: Arc::new(Inner {
            config: config.clone(),
            resource_attributes,
            resource,
            tracing,
            noop_tracers: SdkTracerProvider::builder()
                .with_sampler(Sampler::AlwaysOff)
                .build(),
            meter_provider,
            noop_meters: SdkMeterProvider::builder().build(),
            log_client,
            runtime,
            shutdown: AtomicBool::new(false),
        }),
    })
}

/// A [`LogClient`] that drops every record (logs disabled).
struct DropClient;

#[async_trait]
impl LogClient for DropClient {
    fn send(&self, _entry: LogEntry) {}
    async fn flush(&self, _timeout: Option<Duration>) -> Result<()> {
        Ok(())
    }
    async fn close(&self) -> Result<()> {
        Ok(())
    }
    fn stats(&self) -> BatchStats {
        BatchStats::default()
    }
    fn logger(&self, name: Option<&str>, mut attributes: Attributes) -> Logger {
        if let Some(n) = name {
            attributes.insert("logger.name".into(), n.into());
        }
        Logger::new(Arc::new(DropClient), attributes)
    }
}

impl TelemetryHandle {
    /// The collector config in use.
    pub fn config(&self) -> &OtelCollectorConfig {
        &self.inner.config
    }

    /// Attributes of the shared resource (detected < config attributes < service fields).
    pub fn resource_attributes(&self) -> &Attributes {
        &self.inner.resource_attributes
    }

    /// The shared resource (with the SDK default attributes).
    pub fn resource(&self) -> &Resource {
        &self.inner.resource
    }

    /// The tracing handle, when traces are enabled.
    pub fn tracing(&self) -> Option<&TracingHandle> {
        self.inner.tracing.as_ref()
    }

    /// The tracer provider, when traces are enabled.
    pub fn tracer_provider(&self) -> Option<SdkTracerProvider> {
        self.inner
            .tracing
            .as_ref()
            .map(TracingHandle::tracer_provider)
    }

    /// A tracer (it records nothing when traces are disabled). An empty name means the
    /// service name.
    pub fn tracer(&self, name: impl Into<Cow<'static, str>>, version: Option<&str>) -> SdkTracer {
        let mut name: Cow<'static, str> = name.into();
        if name.is_empty() {
            name = Cow::Owned(self.inner.config.service_name().to_owned());
        }
        match &self.inner.tracing {
            Some(t) => t.tracer(name, version),
            None => self
                .inner
                .noop_tracers
                .tracer_with_scope(InstrumentationScope::builder(name).build()),
        }
    }

    /// The meter provider, when metrics are enabled.
    pub fn meter_provider(&self) -> Option<SdkMeterProvider> {
        self.inner.meter_provider.clone()
    }

    fn meters(&self) -> &SdkMeterProvider {
        self.inner
            .meter_provider
            .as_ref()
            .unwrap_or(&self.inner.noop_meters)
    }

    /// A named meter (records nothing when metrics are disabled); an empty name means
    /// [`DEFAULT_METER_NAME`](crate::DEFAULT_METER_NAME).
    pub fn meter(&self, name: &str) -> Meter {
        let p = self.meters();
        meter_of(&|s| p.meter_with_scope(s), name)
    }

    /// A monotonic `u64` counter (OTLP sum, cumulative). `VALIDATION` for an invalid name.
    pub fn counter(&self, name: &str, opts: InstrumentOptions) -> Result<Counter<u64>> {
        let p = self.meters();
        Instruments(&|s| p.meter_with_scope(s)).counter(name, &opts)
    }

    /// A non-monotonic `i64` counter.
    pub fn up_down_counter(
        &self,
        name: &str,
        opts: InstrumentOptions,
    ) -> Result<UpDownCounter<i64>> {
        let p = self.meters();
        Instruments(&|s| p.meter_with_scope(s)).up_down_counter(name, &opts)
    }

    /// An `f64` histogram with `opts.boundaries` as explicit buckets.
    pub fn histogram(&self, name: &str, opts: InstrumentOptions) -> Result<Histogram<f64>> {
        let p = self.meters();
        Instruments(&|s| p.meter_with_scope(s)).histogram(name, &opts)
    }

    /// A synchronous `f64` gauge (last value).
    pub fn gauge(&self, name: &str, opts: InstrumentOptions) -> Result<Gauge<f64>> {
        let p = self.meters();
        Instruments(&|s| p.meter_with_scope(s)).gauge(name, &opts)
    }

    /// The OTLP log client, when logs are enabled (use it with [`LogLayer`](crate::logging)
    /// or [`LogBridge`](crate::logging) adapters).
    pub fn log_client(&self) -> Option<Arc<BatchingLogClient>> {
        self.inner.log_client.clone()
    }

    /// A logger writing to [`log_client`](Self::log_client) (it drops records when logs are
    /// disabled).
    pub fn logger(&self, name: Option<&str>, attributes: Attributes) -> Logger {
        match &self.inner.log_client {
            Some(lc) => lc.logger(name, attributes),
            None => DropClient.logger(name, attributes),
        }
    }

    /// The runtime metrics handle, when runtime metrics are on.
    pub fn runtime_metrics(&self) -> Option<&RuntimeMetricsHandle> {
        self.inner.runtime.as_ref()
    }

    /// Exports every finished span, queued log record and collected metric now. `TIMEOUT`
    /// after `timeout` (`None` = 30 s).
    pub async fn force_flush(&self, timeout: Option<Duration>) -> Result<()> {
        let timeout = timeout.unwrap_or(Duration::from_secs(30));
        let work = async {
            if let Some(t) = &self.inner.tracing {
                t.force_flush(Some(timeout)).await?;
            }
            if let Some(lc) = &self.inner.log_client {
                lc.flush(Some(timeout)).await?;
            }
            if let Some(mp) = self.inner.meter_provider.clone() {
                let joined = tokio::task::spawn_blocking(move || mp.force_flush()).await;
                flatten("metrics flush", joined)?;
            }
            Ok(())
        };
        tokio::time::timeout(timeout, work).await.map_err(|_| {
            Error::timeout(format!(
                "telemetry.forceFlush: timed out after {} ms",
                timeout.as_millis()
            ))
        })?
    }

    /// Flushes and shuts every provider down; idempotent (later calls return `Ok`). Returns
    /// the first error after trying all of them.
    pub async fn shutdown(&self) -> Result<()> {
        if self.inner.shutdown.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        if let Some(r) = &self.inner.runtime {
            r.stop();
        }
        let mut first: Option<Error> = None;
        let mut keep = |r: Result<()>| {
            if let Err(e) = r {
                first.get_or_insert(e);
            }
        };
        if let Some(t) = &self.inner.tracing {
            keep(t.shutdown().await);
        }
        if let Some(lc) = &self.inner.log_client {
            keep(lc.close().await);
        }
        if let Some(mp) = self.inner.meter_provider.clone() {
            let joined = tokio::task::spawn_blocking(move || mp.shutdown()).await;
            keep(flatten("metrics shutdown", joined));
        }
        match first {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Whether [`shutdown`](Self::shutdown) was called.
    pub fn is_shutdown(&self) -> bool {
        self.inner.shutdown.load(Ordering::SeqCst)
    }
}

fn flatten<E: fmt::Display>(
    what: &str,
    joined: std::result::Result<std::result::Result<(), E>, tokio::task::JoinError>,
) -> Result<()> {
    match joined {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(Error::unavailable(format!("{what} failed: {e}"))),
        Err(e) => Err(Error::unavailable(format!("{what} failed: {e}"))),
    }
}
