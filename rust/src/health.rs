// Service code (OpenTelemetry Collector): health_check probe with the zpages fallback.
//! [`CollectorHealthClient`]: `health()` against the collector's `health_check` extension
//! (port 13133) with a zpages fallback for stock stacks, and `wait_until_ready(timeout)`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::config::OtelCollectorConfig;
use crate::core::{poll_until, HealthStatus, HttpTransport, PollOptions, Result};

/// Default `wait_until_ready` timeout (30 s).
pub const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(30);
/// Default `wait_until_ready` poll interval (500 ms).
pub const DEFAULT_READY_INTERVAL: Duration = Duration::from_millis(500);

/// Checks the collector's `health_check` extension with a zpages fallback. Cheap to clone. Get
/// it from [`OtelCollectorClient::collector_health`](crate::OtelCollectorClient::collector_health)
/// or create a standalone one with [`CollectorHealthClient::new`].
#[derive(Debug, Clone)]
pub struct CollectorHealthClient {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    transport: Arc<HttpTransport>,
    health_url: String,
    servicez_url: String,
    interval: Duration,
    owned: bool,
}

pub(crate) fn servicez_url(config: &OtelCollectorConfig) -> String {
    format!(
        "{}/debug/servicez",
        config.zpages_url().as_str().trim_end_matches('/')
    )
}

impl CollectorHealthClient {
    /// A standalone health client for `config` (`health_url`, `zpages_url`). It makes no network
    /// call; [`close`](Self::close) releases it.
    pub fn new(config: &OtelCollectorConfig) -> Result<Self> {
        let transport = Arc::new(HttpTransport::new(config.probe())?);
        for s in config.known_secrets() {
            transport.add_known_secret(&s);
        }
        Ok(Self::from_parts(transport, config, true))
    }

    /// A standalone health client from the process environment.
    pub fn from_env() -> Result<Self> {
        Self::new(&OtelCollectorConfig::from_env()?)
    }

    pub(crate) fn from_parts(
        transport: Arc<HttpTransport>,
        config: &OtelCollectorConfig,
        owned: bool,
    ) -> Self {
        CollectorHealthClient {
            inner: Arc::new(Inner {
                transport,
                health_url: config.health_url().to_string(),
                servicez_url: servicez_url(config),
                interval: DEFAULT_READY_INTERVAL,
                owned,
            }),
        }
    }

    /// Same client with another `wait_until_ready` poll interval (default 500 ms).
    pub fn with_interval(&self, interval: Duration) -> Self {
        CollectorHealthClient {
            inner: Arc::new(Inner {
                transport: Arc::clone(&self.inner.transport),
                health_url: self.inner.health_url.clone(),
                servicez_url: self.inner.servicez_url.clone(),
                interval,
                owned: self.inner.owned,
            }),
        }
    }

    /// Collector health without retries. `200 {"status":"Server available",…}` → `healthy`,
    /// status `ok`, details `source=health_check`, `httpStatus`, `vendorStatus`, `upSince`,
    /// `uptime`; `503` → `unhealthy`. When the `health_check` endpoint is unreachable (stock
    /// stacks bind it to localhost) it falls back to `GET {zpages}/debug/servicez`: 200 →
    /// healthy with `source=zpages`. Never fails for an unhealthy or unreachable collector; only
    /// `CANCELLED` (closed client) and `CONFIG` are errors.
    pub async fn health(&self) -> Result<HealthStatus> {
        let t = &self.inner.transport;
        let resp = t
            .get(&self.inner.health_url)
            .operation("health")
            .ok_status(503)
            .send()
            .await;
        let primary = match resp {
            Err(err) => HealthStatus::from_error(err)?,
            Ok(resp) => {
                let ok = resp.status() < 300;
                let mut hs = if ok {
                    HealthStatus::healthy("ok")
                } else {
                    HealthStatus::unhealthy("unhealthy")
                }
                .with_detail("source", "health_check")
                .with_detail("httpStatus", resp.status().to_string());
                if let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(resp.bytes()) {
                    for (k, v) in map {
                        let text = match v {
                            Value::String(s) => s,
                            Value::Bool(b) => b.to_string(),
                            Value::Number(n) => n.to_string(),
                            _ => continue,
                        };
                        let key = if k == "status" {
                            "vendorStatus".into()
                        } else {
                            k
                        };
                        hs = hs.with_detail(key, t.redact(&text));
                    }
                }
                return Ok(hs);
            }
        };
        if primary.status != "unreachable" {
            return Ok(primary);
        }
        let fallback = t
            .get(&self.inner.servicez_url)
            .operation("health")
            .header("accept", "text/html")
            .send()
            .await;
        match fallback {
            Ok(resp) => Ok(HealthStatus::healthy("ok")
                .with_detail("source", "zpages")
                .with_detail("httpStatus", resp.status().to_string())
                .with_detail(
                    "healthCheckError",
                    primary.details.get("error").cloned().unwrap_or_default(),
                )),
            Err(err) => {
                HealthStatus::from_error(err)?;
                Ok(primary)
            }
        }
    }

    /// Polls [`health`](Self::health) (every 500 ms unless changed with
    /// [`with_interval`](Self::with_interval)) until the collector is healthy and returns that
    /// status. `TIMEOUT` when it is not healthy within `timeout` (zero = 30 s); `CANCELLED` when
    /// the client is closed. Drop the future to cancel.
    pub async fn wait_until_ready(&self, timeout: Duration) -> Result<HealthStatus> {
        let timeout = if timeout.is_zero() {
            DEFAULT_READY_TIMEOUT
        } else {
            timeout
        };
        let opts = PollOptions {
            timeout,
            interval: self.inner.interval,
        };
        poll_until("health.waitUntilReady", opts, || async {
            let hs = self.health().await?;
            Ok(hs.healthy.then_some(hs))
        })
        .await
    }

    /// Releases a standalone client (no-op for one from a client: the client owns it).
    pub fn close(&self) {
        if self.inner.owned {
            self.inner.transport.close();
        }
    }
}
