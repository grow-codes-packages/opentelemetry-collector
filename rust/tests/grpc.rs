// Service tests (OpenTelemetry Collector): OTLP/gRPC ping, log export and telemetry against an
// in-process tonic collector.
//! OTLP/gRPC against a mock collector (tonic server).

mod common;
mod support;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::fast_retry;
use grow_opentelemetry_collector as sdk;
use opentelemetry::trace::{Span as _, Tracer as _};
use opentelemetry_proto::tonic::collector::logs::v1::logs_service_server::{
    LogsService, LogsServiceServer,
};
use opentelemetry_proto::tonic::collector::logs::v1::{
    ExportLogsServiceRequest, ExportLogsServiceResponse,
};
use opentelemetry_proto::tonic::collector::metrics::v1::metrics_service_server::{
    MetricsService, MetricsServiceServer,
};
use opentelemetry_proto::tonic::collector::metrics::v1::{
    ExportMetricsServiceRequest, ExportMetricsServiceResponse,
};
use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::{
    TraceService, TraceServiceServer,
};
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use sdk::logging::{ExportOptions, LogExporter, LogRecord, Severity};
use sdk::tracing::{Protocol, TracingConfig};
use sdk::{ErrorCode, InstrumentOptions, OtlpLogExporter, TelemetryOptions};
use support::attr;
use tonic::codec::CompressionEncoding;
use tonic::{Request, Response, Status};

use sdk::OtelCollectorClient as Client;
use sdk::OtelCollectorConfig as Config;

/// What the mock collector does with the next calls.
#[derive(Default)]
struct State {
    /// Required `authorization` metadata, if any.
    token: Option<String>,
    /// Fail this many calls with UNAVAILABLE first.
    unavailable: AtomicU32,
    /// Fail every call with this status.
    fail: Mutex<Option<Status>>,
    traces: Mutex<Vec<ExportTraceServiceRequest>>,
    logs: Mutex<Vec<ExportLogsServiceRequest>>,
    metrics: Mutex<Vec<ExportMetricsServiceRequest>>,
    calls: AtomicU32,
}

#[derive(Clone)]
struct Mock(Arc<State>);

impl Mock {
    fn check<T>(&self, req: &Request<T>) -> Result<(), Status> {
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(t) = &self.0.token {
            let got = req
                .metadata()
                .get("authorization")
                .and_then(|v| v.to_str().ok());
            if got != Some(t.as_str()) {
                return Err(Status::unauthenticated(
                    "missing or empty authorization header: Authorization",
                ));
            }
        }
        if let Some(s) = self.0.fail.lock().unwrap().clone() {
            return Err(s);
        }
        let n = self.0.unavailable.load(Ordering::SeqCst);
        if n > 0 {
            self.0.unavailable.store(n - 1, Ordering::SeqCst);
            return Err(Status::unavailable("not ready"));
        }
        Ok(())
    }
}

#[tonic::async_trait]
impl TraceService for Mock {
    async fn export(
        &self,
        request: Request<ExportTraceServiceRequest>,
    ) -> Result<Response<ExportTraceServiceResponse>, Status> {
        self.check(&request)?;
        self.0.traces.lock().unwrap().push(request.into_inner());
        Ok(Response::new(ExportTraceServiceResponse::default()))
    }
}

#[tonic::async_trait]
impl LogsService for Mock {
    async fn export(
        &self,
        request: Request<ExportLogsServiceRequest>,
    ) -> Result<Response<ExportLogsServiceResponse>, Status> {
        self.check(&request)?;
        self.0.logs.lock().unwrap().push(request.into_inner());
        Ok(Response::new(ExportLogsServiceResponse::default()))
    }
}

#[tonic::async_trait]
impl MetricsService for Mock {
    async fn export(
        &self,
        request: Request<ExportMetricsServiceRequest>,
    ) -> Result<Response<ExportMetricsServiceResponse>, Status> {
        self.check(&request)?;
        self.0.metrics.lock().unwrap().push(request.into_inner());
        Ok(Response::new(ExportMetricsServiceResponse::default()))
    }
}

async fn start(state: State) -> (String, Arc<State>) {
    let state = Arc::new(state);
    let mock = Mock(Arc::clone(&state));
    let incoming =
        tonic::transport::server::TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).expect("bind");
    let addr = incoming.local_addr().unwrap();
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(
                TraceServiceServer::new(mock.clone()).accept_compressed(CompressionEncoding::Gzip),
            )
            .add_service(
                LogsServiceServer::new(mock.clone()).accept_compressed(CompressionEncoding::Gzip),
            )
            .add_service(
                MetricsServiceServer::new(mock).accept_compressed(CompressionEncoding::Gzip),
            )
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });
    (format!("http://{addr}"), state)
}

fn grpc_config(url: &str) -> sdk::OtelCollectorConfigBuilder {
    Config::builder()
        .protocol(Protocol::Grpc)
        .grpc_url(url)
        .retry(fast_retry(0.0))
        .request_timeout(Duration::from_secs(5))
}

#[tokio::test]
async fn grpc_ping() {
    let (url, state) = start(State {
        token: Some("Bearer grpc-secret-1".into()),
        ..Default::default()
    })
    .await;
    let ok = Client::new(
        grpc_config(&url)
            .header("Authorization", "Bearer grpc-secret-1")
            .build()
            .unwrap(),
    )
    .unwrap();
    assert!(ok.ping().await.unwrap() < Duration::from_secs(5));
    assert_eq!(state.traces.lock().unwrap().len(), 1);
    assert!(state.traces.lock().unwrap()[0].resource_spans.is_empty());

    let none = Client::new(
        grpc_config(&url)
            .compression(sdk::tracing::Compression::None)
            .build()
            .unwrap(),
    )
    .unwrap();
    let err = none.ping().await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Authentication);
    assert!(err.message().contains("(gRPC 16)"), "{err}");
    assert!(err.message().contains("authorization header"), "{err}");

    // UNAVAILABLE is retried.
    state.unavailable.store(2, Ordering::SeqCst);
    ok.ping().await.unwrap();
    state.unavailable.store(10, Ordering::SeqCst);
    let err = ok.ping().await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Unavailable);
    assert!(err.is_retryable());
    state.unavailable.store(0, Ordering::SeqCst);

    // Closed clients do not call.
    ok.close().await;
    assert_eq!(ok.ping().await.unwrap_err().code(), ErrorCode::Cancelled);
}

#[tokio::test]
async fn grpc_ping_failures() {
    let (url, state) = start(State::default()).await;
    *state.fail.lock().unwrap() = Some(Status::unimplemented("unknown method Nope"));
    let c = Client::new(grpc_config(&url).build().unwrap()).unwrap();
    assert_eq!(c.ping().await.unwrap_err().code(), ErrorCode::NotFound);
    *state.fail.lock().unwrap() = Some(Status::internal(
        "grpc: failed to unmarshal the received message",
    ));
    let err = c.ping().await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Validation);
    assert!(!err.is_retryable());

    // A total timeout shorter than the retries ends the call.
    *state.fail.lock().unwrap() = None;
    state.unavailable.store(1000, Ordering::SeqCst);
    let slow = Client::new(
        grpc_config(&url)
            .retry(sdk::RetryPolicy {
                max_retries: 50,
                initial_backoff: Duration::from_millis(50),
                max_backoff: Duration::from_millis(50),
                ..Default::default()
            })
            .total_timeout(Duration::from_millis(150))
            .build()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(slow.ping().await.unwrap_err().code(), ErrorCode::Timeout);

    // Nothing listening → UNAVAILABLE.
    let dead = Client::new(
        grpc_config(&format!("http://127.0.0.1:{}", common::closed_port()))
            .max_retries(0)
            .build()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        dead.ping().await.unwrap_err().code(),
        ErrorCode::Unavailable
    );
}

#[test]
fn grpc_client_needs_a_runtime() {
    let cfg = grpc_config("http://127.0.0.1:4317").build().unwrap();
    assert_eq!(Client::new(cfg).unwrap_err().code(), ErrorCode::Config);
}

#[tokio::test]
async fn grpc_tls_settings() {
    let cfg = grpc_config("https://127.0.0.1:4317")
        .insecure_skip_verify(true)
        .build()
        .unwrap();
    assert_eq!(Client::new(cfg).unwrap_err().code(), ErrorCode::Config);
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    let cfg = grpc_config("https://localhost:4317")
        .ca_cert_path(format!("{dir}/ca.pem"))
        .client_cert(format!("{dir}/client.pem"), format!("{dir}/client.key"))
        .build()
        .unwrap();
    Client::new(cfg.clone()).unwrap();
    OtlpLogExporter::new(&cfg).unwrap();
    let cfg = grpc_config("https://localhost:4317")
        .ca_cert_path("/nonexistent/ca.pem")
        .build()
        .unwrap();
    assert_eq!(Client::new(cfg).unwrap_err().code(), ErrorCode::Config);
}

#[tokio::test]
async fn grpc_log_export() {
    let (url, state) = start(State {
        token: Some("Bearer grpc-secret-2".into()),
        ..Default::default()
    })
    .await;
    let cfg = grpc_config(&url)
        .header("Authorization", "Bearer grpc-secret-2")
        .build()
        .unwrap();
    let exp = OtlpLogExporter::new(&cfg).unwrap();
    let mut r = LogRecord::new(Severity::Warn, "over grpc").with_attribute("n", 7);
    r.resource.insert("service.name".into(), "grpc-svc".into());
    let res = exp
        .export(std::slice::from_ref(&r), ExportOptions::default())
        .await
        .unwrap();
    assert_eq!(res.accepted, 1);
    let res = exp
        .export(std::slice::from_ref(&r), ExportOptions::new(false))
        .await
        .unwrap();
    assert_eq!(res.accepted, 1);
    {
        let logs = state.logs.lock().unwrap();
        assert_eq!(logs.len(), 2);
        let rl = &logs[0].resource_logs[0];
        assert_eq!(
            attr(&rl.resource.as_ref().unwrap().attributes, "service.name").as_deref(),
            Some("grpc-svc")
        );
        let rec = &rl.scope_logs[0].log_records[0];
        assert_eq!(rec.severity_number, 13);
        assert_eq!(attr(&rec.attributes, "n").as_deref(), Some("7"));
    }

    // Status mapping.
    let unauth = OtlpLogExporter::new(&grpc_config(&url).build().unwrap()).unwrap();
    let err = unauth
        .export(std::slice::from_ref(&r), ExportOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Authentication, "{err}");
    *state.fail.lock().unwrap() = Some(Status::resource_exhausted("queue full \"x\""));
    let err = exp
        .export(std::slice::from_ref(&r), ExportOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::RateLimited, "{err}");
    assert!(err.message().contains("queue full"), "{err}");
    assert!(!err.to_string().contains("grpc-secret-2"));
    *state.fail.lock().unwrap() = None;
    exp.shutdown().await.unwrap();
}

#[tokio::test]
async fn grpc_telemetry_round_trip() {
    let (url, state) = start(State::default()).await;
    let cfg = grpc_config(&url)
        .service_name("grpc-telemetry")
        .set_global(false)
        .runtime_metrics(true)
        .build()
        .unwrap();
    let h = sdk::init_telemetry(&cfg).unwrap();
    let mut span = h.tracer("it", Some("1.0")).start("grpc-span");
    span.end();
    h.counter("grpc_requests", InstrumentOptions::default())
        .unwrap()
        .add(2, &[]);
    h.logger(Some("it"), Default::default())
        .info("grpc log", Default::default());
    h.force_flush(Some(Duration::from_secs(10))).await.unwrap();
    assert!(!state.traces.lock().unwrap().is_empty());
    assert!(!state.logs.lock().unwrap().is_empty());
    assert!(!state.metrics.lock().unwrap().is_empty());
    h.shutdown().await.unwrap();

    // init_tracing over gRPC with the client's config.
    let client = Client::new(cfg).unwrap();
    let t = client
        .init_tracing(TracingConfig::new("grpc-tracing").with_set_global(false))
        .unwrap();
    let mut span = t.tracer("it", None).start("traced");
    span.end();
    t.force_flush(Some(Duration::from_secs(10))).await.unwrap();
    let found = state.traces.lock().unwrap().iter().any(|r| {
        r.resource_spans.iter().any(|rs| {
            attr(&rs.resource.as_ref().unwrap().attributes, "service.name").as_deref()
                == Some("grpc-tracing")
        })
    });
    assert!(found);
    let t2 = client
        .init_telemetry(TelemetryOptions {
            logs: Some(false),
            metrics: Some(false),
            ..Default::default()
        })
        .unwrap();
    client.close().await;
    assert!(t.is_shutdown());
    assert!(t2.is_shutdown());
}
