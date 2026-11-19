// Created by sdk-kit 0.1.0; owned by this repository.
//! Integration suite against the real service (Docker): client round trips and the
//! conformance scenarios for `core` and this service's categories.
//!
//! The collector has no query API: `query` and `services` are served by the test-only
//! read-back adapters (Jaeger sidecar, capture files); `tail`, `labels`, `streams` and
//! `dependencies` are not declared (docs/compatibility.md).

use std::sync::Arc;
use std::time::Duration;

use grow_opentelemetry_collector as sdk;
use grow_opentelemetry_collector_integration as support;
use sdk::logging::LogQueryClient;
use sdk::testing::logging::{LoggingHandler, LoggingTarget};
use sdk::testing::tracing::{TracingHandler, TracingTarget};
use sdk::testing::{ConformanceRunner, CoreHandler};
use sdk::tracing::{TraceQueryClient, TracingConfig};
use sdk::{ErrorCode, LogClientOptions};
use support::{conformance_dir, load_bootstrap_env, CaptureLogQuery, JaegerTraceQuery};

use sdk::blocking::OtelCollectorClient as BlockingClient;

use sdk::OtelCollectorClient as Client;
use sdk::OtelCollectorConfig as Config;

fn client() -> Client {
    load_bootstrap_env();
    Client::new(Config::from_env().expect("config from env")).expect("client")
}

#[tokio::test]
async fn client_round_trip() {
    let c = client();
    let health = c.health().await.expect("health");
    assert!(health.healthy, "service not healthy: {health:?}");
    c.ping().await.expect("authenticated ping");
    assert_eq!(
        c.server_version().await.expect("version"),
        support::var("OTELCOL_TEST_SERVER_VERSION")
    );
    // unreachable services report unhealthy instead of failing
    let down = Client::new(
        Config::builder()
            .url("http://127.0.0.1:9")
            .max_retries(0)
            .build()
            .unwrap(),
    )
    .unwrap();
    assert!(!down.health().await.unwrap().healthy);
    assert_eq!(
        down.ping().await.unwrap_err().code(),
        ErrorCode::Unavailable
    );
    c.close().await;
    c.close().await;
}

#[test]
fn blocking_round_trip() {
    load_bootstrap_env();
    let c = BlockingClient::from_env().expect("client");
    assert!(c.health().expect("health").healthy);
    assert!(
        c.wait_until_ready(Duration::from_secs(10))
            .expect("ready")
            .healthy
    );
    c.ping().expect("ping");
    assert!(!c.server_version().expect("version").is_empty());
    assert!(c.block_on(c.async_client().health()).unwrap().healthy);
    c.close();
}

#[tokio::test]
async fn conformance() {
    load_bootstrap_env();
    let service = format!(
        "grow-conformance-rust-{}",
        std::env::var("GROW_TEST_RUN_ID").unwrap_or_else(|_| "local".into())
    );
    let factory_service = service.clone();
    let runner = ConformanceRunner::new(conformance_dir())
        .capabilities(["version", "query", "services"])
        .var("serviceName", service.clone())
        .handler(CoreHandler::new(Client::from_env))
        .handler(TracingHandler::new(move || {
            let service = factory_service.clone();
            async move {
                let mut config = TracingConfig::new(service).with_set_global(false);
                config.batch.scheduled_delay = Duration::from_millis(100);
                let handle = sdk::init_tracing(config)?;
                let query: Arc<dyn TraceQueryClient> = Arc::new(JaegerTraceQuery);
                Ok(TracingTarget {
                    handle,
                    query: Some(query),
                })
            }
        }))
        .handler(LoggingHandler::new(|opts| async move {
            let client = Client::from_env()?;
            let lc = client.log_client(LogClientOptions {
                batch: Some(opts),
                ..Default::default()
            })?;
            let query: Arc<dyn LogQueryClient> = Arc::new(CaptureLogQuery);
            Ok(LoggingTarget {
                client: lc,
                query: Some(query),
            })
        }));
    let report = runner.run().await.expect("scenarios load");
    println!("{report}");
    report.assert_success();
}
