// Skeleton created by sdk-kit 0.1.0; owned by this repository (edit freely).
//! # grow-opentelemetry-collector
//!
//! Rust SDK for [OpenTelemetry Collector](https://grow-codes-packages.github.io/opentelemetry-collector/) by Grow Codes.
//!
//! ```no_run
//! # async fn demo() -> grow_opentelemetry_collector::Result<()> {
//! use grow_opentelemetry_collector::{OtelCollectorClient, OtelCollectorConfig};
//!
//! let client = OtelCollectorClient::new(OtelCollectorConfig::from_env()?)?;
//! let health = client.health().await?;
//! println!("healthy: {}", health.healthy);
//! client.close().await;
//! # Ok(())
//! # }
//! ```
//!
//! Features: `blocking` (a blocking client), `testing` (in-memory fakes and the conformance
//! runner)
//! , `tracing-layer` and `log-bridge` (logging adapters, on by default)
//! , `tower` and `reqwest-middleware` (tracing instrumentation)
//! .
#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod core;

mod client;
mod config;

#[cfg(feature = "blocking")]
#[cfg_attr(docsrs, doc(cfg(feature = "blocking")))]
pub mod blocking;

#[cfg(feature = "testing")]
#[cfg_attr(docsrs, doc(cfg(feature = "testing")))]
pub mod testing;

pub mod logging;
pub mod tracing;

pub use crate::core::{Error, ErrorCode, HealthStatus, Result, RetryPolicy, Secret};

pub use crate::config::DEFAULT_URL;

pub use crate::client::OtelCollectorClient;

pub use crate::config::OtelCollectorConfig;

pub use crate::config::OtelCollectorConfigBuilder;

/// Service-prefixed alias of [`Error`].
pub type OtelCollectorError = Error;
