// Service code (OpenTelemetry Collector): OTLP log export on the official exporters.
//! [`OtlpLogExporter`] (the SPEC §4 [`LogExporter`]) built on the official
//! `opentelemetry-otlp` log exporters (protobuf over HTTP, or gRPC), and [`create_log_client`]
//! that wraps it in the template [`BatchingLogClient`].

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use async_trait::async_trait;
use opentelemetry::logs::{AnyValue, LogRecord as _, Logger as _, LoggerProvider as _};
use opentelemetry::trace::{SpanId, TraceFlags, TraceId};
use opentelemetry::{InstrumentationScope, Key};
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig, WithTonicConfig};
use opentelemetry_sdk::error::OTelSdkError;
use opentelemetry_sdk::logs::{LogBatch, LogExporter as _, SdkLogRecord, SdkLoggerProvider};

use crate::config::{OtelCollectorConfig, Signal};
use crate::core::redact::{redact_text, truncate};
use crate::core::transport::{MAX_VENDOR_MESSAGE, SDK_VERSION};
use crate::core::{Error, Result};
use crate::logging::{
    AttrValue, Attributes, BatchOptions, BatchingLogClient, ExportOptions, ExportResult,
    LogExporter, LogRecord, RedactionProcessor, Severity,
};
use crate::otlp::{
    decode_partial_success, grpc_metadata, grpc_status_error, http_status_error, parse_tonic_debug,
    reqwest_client, tonic_tls, CaptureClient, CaptureSlot, Captured, ClientSettings,
    PartialSuccess,
};
use crate::resource::resource_from_attributes;
use crate::tracing::{Compression, Protocol};

/// Instrumentation scope name of records exported by [`OtlpLogExporter`] and default meter name.
pub const INSTRUMENTATION_SCOPE: &str = "grow-codes-opentelemetry-collector";

/// Exports log records to the collector with the official OpenTelemetry OTLP log exporters
/// (`http/protobuf` to the logs endpoint, or `LogsService/Export` over gRPC), per the config's
/// protocol. Records keep their own resource (one request per distinct resource). A partial
/// success (`rejected_log_records`, HTTP) is reported in [`ExportResult`] and never resent.
/// Error statuses map to SDK errors (`401` → `AUTHENTICATION`, `400` → `VALIDATION`, `429` →
/// `RATE_LIMITED`, `503` → `UNAVAILABLE`, gRPC codes likewise); retryable failures are retried
/// by the [`BatchingLogClient`] processor, never by the exporter itself.
pub struct OtlpLogExporter {
    config: OtelCollectorConfig,
    state: tokio::sync::Mutex<State>,
    slot: CaptureSlot,
    provider: SdkLoggerProvider,
    scope: InstrumentationScope,
    secrets: Vec<String>,
}

#[derive(Default)]
struct State {
    gzip: Option<opentelemetry_otlp::LogExporter>,
    plain: Option<opentelemetry_otlp::LogExporter>,
    shutdown: bool,
}

impl fmt::Debug for OtlpLogExporter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OtlpLogExporter")
            .field(
                "endpoint",
                &crate::core::redact::redact_url(&self.endpoint()),
            )
            .field("protocol", &self.config.protocol())
            .finish_non_exhaustive()
    }
}

const OP: &str = "logs.export";

impl OtlpLogExporter {
    /// An exporter for `config` (`logs_url`, or `url + /v1/logs`, or the gRPC target). It makes
    /// no network call (connections are opened on the first export); TLS files are checked now.
    pub fn new(config: &OtelCollectorConfig) -> Result<Self> {
        config.validate()?;
        let exporter = OtlpLogExporter {
            config: config.clone(),
            state: tokio::sync::Mutex::new(State::default()),
            slot: Arc::new(Mutex::new(None)),
            provider: SdkLoggerProvider::builder().build(),
            scope: InstrumentationScope::builder(INSTRUMENTATION_SCOPE)
                .with_version(SDK_VERSION)
                .build(),
            secrets: config
                .known_secrets()
                .iter()
                .map(|s| s.expose_secret().to_owned())
                .collect(),
        };
        match config.protocol() {
            Protocol::Grpc => {
                tonic_tls(&exporter.endpoint(), config.common().tls())?;
            }
            _ => {
                reqwest_client(&exporter.client_settings())?;
            }
        }
        Ok(exporter)
    }

    /// The logs endpoint in use.
    pub fn endpoint(&self) -> String {
        self.config.signal_endpoint(Signal::Logs)
    }

    fn client_settings(&self) -> ClientSettings {
        let c = self.config.common();
        ClientSettings {
            tls: c.tls().clone(),
            proxy: c.proxy().map(str::to_owned),
            connect_timeout: Some(c.connect_timeout()),
            timeout: self.config.export_timeout(),
        }
    }

    fn build(&self, gzip: bool) -> Result<opentelemetry_otlp::LogExporter> {
        let endpoint = self.endpoint();
        let headers = self.config.signal_headers(Signal::Logs);
        let gzip = gzip && self.config.compression() == Compression::Gzip;
        let built = match self.config.protocol() {
            Protocol::Grpc => {
                let mut b = opentelemetry_otlp::LogExporter::builder()
                    .with_tonic()
                    .with_endpoint(endpoint.clone())
                    .with_timeout(self.config.export_timeout())
                    .with_metadata(grpc_metadata(&headers)?);
                if gzip {
                    b = b.with_compression(opentelemetry_otlp::Compression::Gzip);
                }
                if let Some(tls) = tonic_tls(&endpoint, self.config.common().tls())? {
                    b = b.with_tls_config(tls);
                }
                b.build()
            }
            _ => {
                let client = CaptureClient::new(
                    reqwest_client(&self.client_settings())?,
                    Arc::clone(&self.slot),
                );
                let mut b = opentelemetry_otlp::LogExporter::builder()
                    .with_http()
                    .with_endpoint(endpoint)
                    .with_timeout(self.config.export_timeout())
                    .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
                    .with_headers(headers.into_iter().collect::<HashMap<_, _>>())
                    .with_http_client(client);
                if gzip {
                    b = b.with_compression(opentelemetry_otlp::Compression::Gzip);
                }
                b.build()
            }
        };
        built.map_err(|e| Error::config(format!("{OP}: cannot build the OTLP log exporter: {e}")))
    }

    fn record(&self, r: &LogRecord, now: SystemTime) -> SdkLogRecord {
        let logger = self.provider.logger_with_scope(self.scope.clone());
        let mut rec = logger.create_log_record();
        rec.set_timestamp(r.timestamp);
        rec.set_observed_timestamp(r.observed_timestamp.unwrap_or(now));
        rec.set_severity_number(otel_severity(r.severity));
        rec.set_severity_text(severity_text(r.severity, &r.severity_text));
        rec.set_body(AnyValue::String(r.body.clone().into()));
        for (k, v) in &r.attributes {
            if let Some(v) = any_value(v) {
                rec.add_attribute(Key::new(k.clone()), v);
            }
        }
        let trace = r
            .trace_id
            .as_deref()
            .and_then(|t| TraceId::from_hex(t).ok());
        let span = r.span_id.as_deref().and_then(|s| SpanId::from_hex(s).ok());
        if let (Some(t), Some(s)) = (trace, span) {
            if t != TraceId::INVALID && s != SpanId::INVALID {
                rec.set_trace_context(t, s, r.trace_flags.map(TraceFlags::new));
            }
        }
        rec
    }

    fn map_result(&self, result: std::result::Result<(), OTelSdkError>) -> Result<PartialSuccess> {
        let captured = self.slot.lock().unwrap_or_else(|e| e.into_inner()).take();
        match captured {
            Some(Captured::Response {
                status,
                content_type,
                retry_after,
                body,
            }) => {
                if (200..300).contains(&status) {
                    return Ok(decode_partial_success(&body));
                }
                Err(http_status_error(
                    OP,
                    status,
                    content_type.as_deref(),
                    retry_after.as_deref(),
                    &body,
                    &self.secrets,
                ))
            }
            Some(Captured::Failed { timeout }) => Err(if timeout {
                Error::timeout(format!(
                    "{OP}: export timed out after {} ms",
                    self.config.export_timeout().as_millis()
                ))
            } else {
                Error::unavailable(format!("{OP}: connection failed"))
            }),
            None => match result {
                Ok(()) => Ok(PartialSuccess::default()),
                Err(OTelSdkError::AlreadyShutdown) => {
                    Err(Error::cancelled(format!("{OP}: exporter is shut down")))
                }
                Err(OTelSdkError::Timeout(d)) => Err(Error::timeout(format!(
                    "{OP}: export timed out after {} ms",
                    d.as_millis()
                ))),
                Err(OTelSdkError::InternalFailure(text)) => Err(match parse_tonic_debug(&text) {
                    Some((code, message)) => grpc_status_error(OP, code, &message, &self.secrets),
                    None => Error::unavailable(format!(
                        "{OP}: {}",
                        truncate(&redact_text(&text, &self.secrets), MAX_VENDOR_MESSAGE)
                    )),
                }),
            },
        }
    }
}

#[async_trait]
impl LogExporter for OtlpLogExporter {
    /// Sends `records` in one OTLP request per distinct resource. `accepted = n - rejected`.
    async fn export(&self, records: &[LogRecord], opts: ExportOptions) -> Result<ExportResult> {
        let mut state = self.state.lock().await;
        if state.shutdown {
            return Err(Error::cancelled(format!("{OP}: exporter is shut down")));
        }
        if records.is_empty() {
            return Ok(ExportResult::accepted(0));
        }
        let slot = if opts.gzip {
            &mut state.gzip
        } else {
            &mut state.plain
        };
        if slot.is_none() {
            *slot = Some(self.build(opts.gzip)?);
        }
        let exporter = slot.as_mut().expect("exporter just built");
        // Group by resource, first-seen order.
        let mut order: Vec<String> = Vec::new();
        let mut groups: HashMap<String, (&Attributes, Vec<&LogRecord>)> = HashMap::new();
        for r in records {
            let key = serde_json::Value::Object(
                r.resource
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            )
            .to_string();
            groups
                .entry(key.clone())
                .or_insert_with(|| {
                    order.push(key);
                    (&r.resource, Vec::new())
                })
                .1
                .push(r);
        }
        let now = SystemTime::now();
        let mut rejected = 0i64;
        let mut messages = Vec::new();
        for key in order {
            let (resource, recs) = &groups[&key];
            let built: Vec<SdkLogRecord> = recs.iter().map(|r| self.record(r, now)).collect();
            let pairs: Vec<(&SdkLogRecord, &InstrumentationScope)> =
                built.iter().map(|r| (r, &self.scope)).collect();
            exporter.set_resource(&resource_from_attributes(resource));
            self.slot.lock().unwrap_or_else(|e| e.into_inner()).take();
            let result = exporter.export(LogBatch::new(&pairs)).await;
            let ps = self.map_result(result)?;
            rejected += ps.rejected.max(0);
            if !ps.message.is_empty() {
                messages.push(ps.message);
            }
        }
        let n = usize::try_from(rejected)
            .unwrap_or(usize::MAX)
            .min(records.len());
        if n == 0 && messages.is_empty() {
            return Ok(ExportResult::accepted(records.len()));
        }
        let mut msg = format!("{OP}: {n} of {} records rejected", records.len());
        if let Some(m) = messages.first() {
            msg.push_str(": ");
            msg.push_str(&truncate(
                &redact_text(m, &self.secrets),
                MAX_VENDOR_MESSAGE,
            ));
        }
        Ok(ExportResult::partial(
            records.len() - n,
            n,
            Some(Error::validation(msg)),
        ))
    }

    /// Shuts the official exporters down; later exports fail with `CANCELLED`. Idempotent.
    async fn shutdown(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        if state.shutdown {
            return Ok(());
        }
        state.shutdown = true;
        for exp in [state.gzip.take(), state.plain.take()]
            .into_iter()
            .flatten()
        {
            let _ = exp.shutdown();
        }
        Ok(())
    }
}

fn otel_severity(s: Severity) -> opentelemetry::logs::Severity {
    use opentelemetry::logs::Severity as O;
    match s {
        Severity::Trace => O::Trace,
        Severity::Debug => O::Debug,
        Severity::Info => O::Info,
        Severity::Warn => O::Warn,
        Severity::Error => O::Error,
        Severity::Fatal => O::Fatal,
    }
}

/// The OpenTelemetry SDK keeps severity texts as `&'static str`: a canonical text (`TRACE` …
/// `FATAL`, case-insensitive) is kept, any other text becomes the severity's canonical text.
fn severity_text(s: Severity, text: &str) -> &'static str {
    Severity::ALL
        .iter()
        .map(|x| x.text())
        .find(|t| t.eq_ignore_ascii_case(text.trim()))
        .unwrap_or_else(|| s.text())
}

/// Converts an attribute value to an OpenTelemetry log `AnyValue` (maps and arrays keep their
/// structure; `null` is dropped).
pub(crate) fn any_value(v: &AttrValue) -> Option<AnyValue> {
    Some(match v {
        AttrValue::Null => return None,
        AttrValue::Bool(b) => AnyValue::Boolean(*b),
        AttrValue::Number(n) => match n.as_i64() {
            Some(i) => AnyValue::Int(i),
            None => AnyValue::Double(n.as_f64().unwrap_or(f64::NAN)),
        },
        AttrValue::String(s) => AnyValue::String(s.clone().into()),
        AttrValue::Array(items) => {
            AnyValue::ListAny(Box::new(items.iter().filter_map(any_value).collect()))
        }
        AttrValue::Object(map) => AnyValue::Map(Box::new(
            map.iter()
                .filter_map(|(k, v)| any_value(v).map(|v| (Key::new(k.clone()), v)))
                .collect(),
        )),
    })
}

/// Options of [`create_log_client`] and
/// [`OtelCollectorClient::log_client`](crate::OtelCollectorClient::log_client).
#[derive(Debug, Clone, Default)]
pub struct LogClientOptions {
    /// Batching (default: the config's `log_batch`, i.e. `OTEL_BLRP_*`, then SPEC §4).
    pub batch: Option<BatchOptions>,
    /// Resource of every record (default: the config's resource: `service.name`,
    /// `service.version`, `deployment.environment.name` and `resource_attributes`).
    pub resource: Option<Attributes>,
    /// Redaction before export (default [`RedactionProcessor::default`]); `Some(None)`
    /// disables it.
    pub redaction: Option<Option<RedactionProcessor>>,
    /// Minimum severity of `logger()` loggers (default `Info`).
    pub min_severity: Option<Severity>,
}

/// A [`BatchingLogClient`] on an [`OtlpLogExporter`] for `config`. Must be called inside a tokio
/// runtime; `close()` flushes it and shuts the exporter down.
pub fn create_log_client(
    config: &OtelCollectorConfig,
    opts: LogClientOptions,
) -> Result<BatchingLogClient> {
    let exporter = Arc::new(OtlpLogExporter::new(config)?);
    let mut b = BatchingLogClient::builder(exporter)
        .batch_options(opts.batch.unwrap_or_else(|| config.log_batch().clone()))
        .resource(opts.resource.unwrap_or_else(|| config.resource_map()));
    if let Some(r) = opts.redaction {
        b = b.redaction(r);
    }
    if let Some(s) = opts.min_severity {
        b = b.min_severity(s);
    }
    b.build()
}
