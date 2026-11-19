// Created by sdk-kit 0.1.0; owned by this repository.
//! Sends traces, metrics and logs to an OpenTelemetry Collector with the Grow Codes Rust SDK:
//! configuration from the environment, health (with the zpages fallback), ping, server
//! version, the one-call telemetry bootstrap and the log client.
//!
//! ```sh
//! GROW_OPENTELEMETRY_COLLECTOR_URL=http://127.0.0.1:4318 OTEL_SERVICE_NAME=example cargo run
//! ```

use std::sync::Arc;
use std::time::Duration;

use grow_opentelemetry_collector as sdk;
use sdk::core::{ErrorInfo, Hooks};
use sdk::logging::attrs;
use sdk::opentelemetry::trace::{Span as _, Tracer as _};
use sdk::{ErrorCode, InstrumentOptions, TelemetryOptions};

use sdk::OtelCollectorClient as Client;
use sdk::OtelCollectorConfig as Config;

/// Prints every retry of health, ping and version calls.
struct RetryLogger;

impl Hooks for RetryLogger {
    fn on_error(&self, info: &ErrorInfo) {
        if info.will_retry {
            eprintln!(
                "retrying {} {} (attempt {})",
                info.method, info.url, info.attempt
            );
        }
    }
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("error [{}]: {err}", err.code());
        std::process::exit(1);
    }
}

async fn run() -> sdk::Result<()> {
    // explicit > GROW_OPENTELEMETRY_COLLECTOR_* > GROW_OTEL_* > OTEL_* > defaults; invalid values
    // name the variable, never its value.
    let config = Config::builder()
        .hooks(Arc::new(RetryLogger))
        .build_from_env()?;
    println!("config: {config:?}"); // header values such as Authorization print as [REDACTED]

    let client = Client::new(config)?;
    let health = client.wait_until_ready(Duration::from_secs(10)).await?;
    println!(
        "health: healthy={} status={} source={}",
        health.healthy,
        health.status,
        health
            .details
            .get("source")
            .map(String::as_str)
            .unwrap_or("-")
    );

    match client.ping().await {
        Ok(latency) => println!("ping: {} ms", latency.as_millis()),
        Err(e) if e.code() == ErrorCode::Authentication => {
            println!("ping: the collector rejected the configured headers ({e})");
            return Err(e);
        }
        Err(e) => return Err(e),
    }
    match client.server_version().await {
        Ok(version) => println!("collector version: {version}"),
        Err(e) if e.code() == ErrorCode::Unsupported => println!("collector version: not exposed"),
        Err(e) => return Err(e),
    }

    // Traces, metrics and logs with one shared resource; client.close() shuts it down.
    let telemetry = client.init_telemetry(TelemetryOptions::default())?;
    let tracer = telemetry.tracer("example", None);
    let mut span = tracer.start("checkout");
    span.set_attribute(sdk::opentelemetry::KeyValue::new("order.id", 42));
    let trace_id = span.span_context().trace_id().to_string();
    span.end();
    telemetry
        .counter(
            "example_orders",
            InstrumentOptions::default().unit("{order}"),
        )?
        .add(1, &[]);
    telemetry
        .histogram(
            "example_latency",
            InstrumentOptions::default()
                .unit("ms")
                .boundaries(vec![10.0, 100.0]),
        )?
        .record(42.0, &[]);
    telemetry
        .logger(Some("example"), attrs! {"component" => "checkout"})
        .info(
            "order placed",
            attrs! {"order.id" => 42, "password" => "redacted"},
        );
    telemetry.force_flush(Some(Duration::from_secs(10))).await?;
    println!("sent trace {trace_id}, two metrics and one log record");

    client.close().await;
    Ok(())
}
