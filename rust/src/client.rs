// Service code (OpenTelemetry Collector): the client entry point.
//! `OtelCollectorClient`: entry point of the SDK (SPEC §2.4): health (health_check with a
//! zpages fallback), ping (an empty OTLP export), server version (zpages, then the collector's
//! own `target_info` metric), the OTLP log exporter and log client, `init_telemetry`,
//! `init_tracing` and the (unsupported) query clients.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use opentelemetry_proto::tonic::collector::trace::v1::trace_service_client::TraceServiceClient;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use regex::Regex;
use tonic::codec::CompressionEncoding;
use tonic::transport::{Channel, Endpoint};

use crate::config::{OtelCollectorConfig, Signal};
use crate::core::lifecycle::CloseGroup;
use crate::core::transport::default_user_agent;
use crate::core::{Error, ErrorCode, HealthStatus, HttpTransport, Result};
use crate::health::{servicez_url, CollectorHealthClient};
use crate::logging::{BatchingLogClient, LogClient as _};
use crate::logs::{create_log_client, LogClientOptions, OtlpLogExporter};
use crate::otlp::{grpc_metadata, http_status_error, tonic_error, tonic_tls};
use crate::query::{CollectorLogQueryClient, CollectorTraceQueryClient};
use crate::setup::init_tracing_with_collector;
use crate::telemetry::{init_telemetry_with, TelemetryHandle, TelemetryOptions};
use crate::tracing::{Protocol, TracingConfig, TracingHandle};

/// Async client for the OpenTelemetry Collector. Cheap to clone (clones share one connection
/// pool). `Debug` output redacts every credential.
#[derive(Clone)]
pub struct OtelCollectorClient {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for OtelCollectorClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OtelCollectorClient")
            .field("config", &self.inner.config)
            .field("grpc", &self.inner.grpc.is_some())
            .field("closed", &self.is_closed())
            .finish()
    }
}

struct Inner {
    config: OtelCollectorConfig,
    transport: HttpTransport,
    probe: Arc<HttpTransport>,
    health: CollectorHealthClient,
    grpc: Option<GrpcPing>,
    secrets: Vec<String>,
    on_close: CloseGroup,
}

/// The lazily connecting gRPC channel of `ping()` over OTLP/gRPC.
#[derive(Debug)]
struct GrpcPing {
    channel: Channel,
}

fn servicez_version(html: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"<b>Version</b></td>\s*<td>[^<]*</td>\s*<td[^>]*>([^<]+)</td>").expect("regex")
    });
    re.captures(html)
        .map(|c| c[1].trim().trim_start_matches('v').to_owned())
        .filter(|v| !v.is_empty())
}

fn target_info_version(metrics: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r#"target_info\{[^}]*service_version="([^"]+)""#).expect("regex")
    });
    re.captures(metrics)
        .map(|c| c[1].trim_start_matches('v').to_owned())
}

/// Parses the collector version from the zpages `/debug/servicez` page (`None` when absent).
pub fn parse_servicez_version(html: &str) -> Option<String> {
    servicez_version(html)
}

/// Parses `service_version` of the `target_info` metric in a Prometheus exposition.
pub fn parse_target_info_version(metrics: &str) -> Option<String> {
    target_info_version(metrics)
}

impl OtelCollectorClient {
    /// Creates a client from a configuration. It makes no network call; the gRPC channel of
    /// `ping()` (protocol `grpc`) connects lazily, so this must run inside a tokio runtime
    /// for that protocol.
    pub fn new(config: OtelCollectorConfig) -> Result<Self> {
        config.validate()?;
        let transport = HttpTransport::new(config.common())?;
        let probe = Arc::new(HttpTransport::new(config.probe())?);
        let known = config.known_secrets();
        for s in &known {
            transport.add_known_secret(s);
            probe.add_known_secret(s);
        }
        let secrets = known.iter().map(|s| s.expose_secret().to_owned()).collect();
        let grpc = match config.protocol() {
            Protocol::Grpc => Some(GrpcPing::new(&config)?),
            _ => None,
        };
        let health = CollectorHealthClient::from_parts(Arc::clone(&probe), &config, false);
        Ok(OtelCollectorClient {
            inner: Arc::new(Inner {
                config,
                transport,
                probe,
                health,
                grpc,
                secrets,
                on_close: CloseGroup::new(),
            }),
        })
    }

    /// Creates a client from the process environment.
    pub fn from_env() -> Result<Self> {
        Self::new(OtelCollectorConfig::from_env()?)
    }

    /// The configuration in use.
    pub fn config(&self) -> &OtelCollectorConfig {
        &self.inner.config
    }

    /// The HTTP transport of `ping()` (common settings, retries).
    pub fn transport(&self) -> &HttpTransport {
        &self.inner.transport
    }

    /// Registers work that [`OtelCollectorClient::close`] runs (flush, shutdown, …).
    pub fn on_close<F>(&self, f: F)
    where
        F: FnOnce() -> BoxFuture<'static, ()> + Send + 'static,
    {
        self.inner.on_close.register(f);
    }

    /// The health client of this collector (shares the client's connections).
    pub fn collector_health(&self) -> CollectorHealthClient {
        self.inner.health.clone()
    }

    /// Unauthenticated liveness check, see [`CollectorHealthClient::health`]. Never fails for
    /// an unhealthy or unreachable collector; only `CANCELLED` and `CONFIG` are returned as
    /// errors.
    pub async fn health(&self) -> Result<HealthStatus> {
        self.inner.health.health().await
    }

    /// Polls `health()` until the collector is healthy, see
    /// [`CollectorHealthClient::wait_until_ready`].
    pub async fn wait_until_ready(&self, timeout: Duration) -> Result<HealthStatus> {
        self.inner.health.wait_until_ready(timeout).await
    }

    /// Authenticated round trip; returns the latency. It sends an empty OTLP trace export
    /// (`POST <traces endpoint>` with an empty protobuf body, or `TraceService/Export` with an
    /// empty message over gRPC); the collector stores nothing for it. `AUTHENTICATION` /
    /// `PERMISSION_DENIED` when an authenticator or gateway rejects the headers, `NOT_FOUND`
    /// for a wrong path.
    pub async fn ping(&self) -> Result<Duration> {
        const OP: &str = "ping";
        if self.is_closed() {
            return Err(Error::cancelled(format!("{OP}: client is closed")));
        }
        let started = Instant::now();
        if let Some(g) = &self.inner.grpc {
            g.ping(&self.inner.config, &self.inner.secrets).await?;
            return Ok(started.elapsed());
        }
        let endpoint = self.inner.config.signal_endpoint(Signal::Traces);
        let mut req = self
            .transport()
            .post(&endpoint)
            .operation(OP)
            .body(bytes::Bytes::new(), "application/x-protobuf")
            .header("accept", "application/x-protobuf");
        for (k, v) in self.inner.config.signal_only_headers(Signal::Traces) {
            req = req.header(k.clone(), v.clone());
        }
        // 429 stays with the transport (it retries POST on 429); other statuses are mapped
        // here because OTLP error bodies are protobuf.
        for status in (400..600).filter(|s| *s != 429) {
            req = req.ok_status(status);
        }
        let resp = req.send().await?;
        if resp.status() >= 400 {
            return Err(http_status_error(
                OP,
                resp.status(),
                resp.header("content-type"),
                resp.header("retry-after"),
                resp.bytes(),
                &self.inner.secrets,
            ));
        }
        Ok(started.elapsed())
    }

    /// Collector version (for example `0.165.0`, without a leading `v`). It parses the
    /// `Version` row of the zpages page `{zpages_url}/debug/servicez` (reachable on stock
    /// stacks) and falls back to the `target_info` metric of `telemetry_url`. `UNSUPPORTED`
    /// when neither is reachable or carries a version. The version is never derived from OTLP
    /// responses or the image tag.
    pub async fn server_version(&self) -> Result<String> {
        const OP: &str = "serverVersion";
        let fatal = |e: Error| {
            matches!(
                e.code(),
                ErrorCode::Cancelled | ErrorCode::Authentication | ErrorCode::PermissionDenied
            )
            .then_some(e)
        };
        let page = self
            .inner
            .probe
            .get(&servicez_url(&self.inner.config))
            .operation(OP)
            .header("accept", "text/html")
            .send()
            .await;
        match page {
            Ok(resp) => {
                if let Some(v) = servicez_version(&String::from_utf8_lossy(resp.bytes())) {
                    return Ok(v);
                }
            }
            Err(e) => {
                if let Some(e) = fatal(e) {
                    return Err(e);
                }
            }
        }
        if let Some(url) = self.inner.config.telemetry_url() {
            let metrics = self
                .inner
                .probe
                .get(url.as_str())
                .operation(OP)
                .header("accept", "text/plain")
                .send()
                .await;
            match metrics {
                Ok(resp) => {
                    if let Some(v) = target_info_version(&String::from_utf8_lossy(resp.bytes())) {
                        return Ok(v);
                    }
                }
                Err(e) => {
                    if let Some(e) = fatal(e) {
                        return Err(e);
                    }
                }
            }
        }
        Err(Error::unsupported(format!(
            "{OP}: neither zpages nor the telemetry endpoint exposes a version"
        )))
    }

    /// The trace query client. The collector stores nothing, so every method returns
    /// `UNSUPPORTED` (read traces from the backend the collector exports to).
    pub fn trace_query(&self) -> CollectorTraceQueryClient {
        CollectorTraceQueryClient
    }

    /// The log query client. The collector stores nothing, so every method returns
    /// `UNSUPPORTED`.
    pub fn log_query(&self) -> CollectorLogQueryClient {
        CollectorLogQueryClient
    }

    /// A new OTLP log exporter for the client's configuration. [`close`](Self::close) shuts
    /// it down; `CANCELLED` after `close`.
    pub fn log_exporter(&self) -> Result<Arc<OtlpLogExporter>> {
        self.check_open("log_exporter")?;
        let e = Arc::new(OtlpLogExporter::new(&self.inner.config)?);
        let closer = Arc::clone(&e);
        self.on_close(move || {
            Box::pin(async move {
                let _ = crate::logging::LogExporter::shutdown(&*closer).await;
            })
        });
        Ok(e)
    }

    /// A [`BatchingLogClient`] that exports OTLP logs to the collector (see
    /// [`create_log_client`]). [`close`](Self::close) flushes and closes it.
    pub fn log_client(&self, opts: LogClientOptions) -> Result<Arc<BatchingLogClient>> {
        self.check_open("log_client")?;
        let lc = Arc::new(create_log_client(&self.inner.config, opts)?);
        let closer = Arc::clone(&lc);
        self.on_close(move || {
            Box::pin(async move {
                let _ = closer.close().await;
            })
        });
        Ok(lc)
    }

    /// Starts traces, metrics and logs with the client's configuration (see
    /// [`init_telemetry_with`]). [`close`](Self::close) shuts the handle down.
    pub fn init_telemetry(&self, opts: TelemetryOptions) -> Result<TelemetryHandle> {
        self.check_open("init_telemetry")?;
        let h = init_telemetry_with(&self.inner.config, opts)?;
        let closer = h.clone();
        self.on_close(move || {
            Box::pin(async move {
                let _ = closer.shutdown().await;
            })
        });
        Ok(h)
    }

    /// Starts tracing with the client's configuration as the defaults of `config` (see
    /// [`tracing_config_from`](crate::tracing_config_from)). [`close`](Self::close) shuts the
    /// handle down.
    pub fn init_tracing(&self, config: TracingConfig) -> Result<TracingHandle> {
        self.check_open("init_tracing")?;
        let h = init_tracing_with_collector(config, &self.inner.config)?;
        let closer = h.clone();
        self.on_close(move || {
            Box::pin(async move {
                let _ = closer.shutdown().await;
            })
        });
        Ok(h)
    }

    /// Flushes and shuts down every log client, exporter and telemetry handle created through
    /// the client, then closes its connections. Idempotent.
    pub async fn close(&self) {
        self.inner.on_close.close().await;
        self.inner.transport.close();
        self.inner.probe.close();
    }

    /// `CANCELLED` once [`close`](Self::close) was called (same rule in every language).
    fn check_open(&self, operation: &str) -> Result<()> {
        if self.is_closed() || self.inner.on_close.is_closed() {
            return Err(Error::cancelled(format!("{operation}: client is closed")));
        }
        Ok(())
    }

    /// Whether [`OtelCollectorClient::close`] was called.
    pub fn is_closed(&self) -> bool {
        self.inner.transport.is_closed()
    }
}

impl GrpcPing {
    fn new(config: &OtelCollectorConfig) -> Result<Self> {
        let target = config.signal_endpoint(Signal::Traces);
        let mut endpoint = Endpoint::from_shared(target.clone())
            .map_err(|_| Error::config("grpc endpoint is not a valid URL"))?
            .connect_timeout(config.common().connect_timeout())
            .timeout(config.common().request_timeout());
        let ua = match config.common().user_agent_suffix() {
            Some(s) if !s.is_empty() => format!("{} {s}", default_user_agent()),
            _ => default_user_agent(),
        };
        endpoint = endpoint
            .user_agent(ua)
            .map_err(|_| Error::config("user agent is not a valid header value"))?;
        if let Some(tls) = tonic_tls(&target, config.common().tls())? {
            endpoint = endpoint
                .tls_config(tls)
                .map_err(|e| Error::config("invalid gRPC TLS settings").with_source(e))?;
        }
        tokio::runtime::Handle::try_current()
            .map_err(|_| Error::config("a gRPC client must be created inside a tokio runtime"))?;
        Ok(GrpcPing {
            channel: endpoint.connect_lazy(),
        })
    }

    async fn ping(&self, config: &OtelCollectorConfig, secrets: &[String]) -> Result<()> {
        const OP: &str = "ping";
        let metadata = grpc_metadata(&config.signal_headers(Signal::Traces))?;
        let mut client = TraceServiceClient::new(self.channel.clone());
        if config.compression() == crate::tracing::Compression::Gzip {
            client = client.send_compressed(CompressionEncoding::Gzip);
        }
        client = client.accept_compressed(CompressionEncoding::Gzip);
        let policy = config.common().retry().clone();
        let call = async {
            let mut attempt = 0u32;
            loop {
                let mut req = tonic::Request::new(ExportTraceServiceRequest::default());
                *req.metadata_mut() = metadata.clone();
                match client.export(req).await {
                    Ok(_) => return Ok(()),
                    Err(status) => {
                        let err = tonic_error(OP, &status, secrets);
                        if status.code() != tonic::Code::Unavailable
                            || attempt >= policy.max_retries
                        {
                            return Err(err);
                        }
                        tokio::time::sleep(policy.backoff(attempt)).await;
                        attempt += 1;
                    }
                }
            }
        };
        match tokio::time::timeout(config.common().total_timeout(), call).await {
            Ok(r) => r,
            Err(_) => Err(Error::timeout(format!(
                "{OP}: total timeout of {} ms exceeded",
                config.common().total_timeout().as_millis()
            ))),
        }
    }
}
