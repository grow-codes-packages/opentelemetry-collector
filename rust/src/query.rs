// Service code (OpenTelemetry Collector): the query clients, which the collector cannot serve.
//! [`CollectorTraceQueryClient`] and [`CollectorLogQueryClient`]: the collector stores nothing
//! and has no query API, so every method returns `UNSUPPORTED`. Read data back from the backend
//! the collector exports to (Jaeger, Tempo, Loki, …).

use async_trait::async_trait;

use crate::core::{Error, Result};
use crate::logging::{
    LogQueryClient, LogRecord, LogSearchResult, RecordCallback, SearchOptions, TailHandle,
    TailOptions, WaitOptions,
};
use crate::tracing::{
    DependencyLink, DependencyOptions, Trace, TraceQueryClient, TraceSearchQuery, TraceSummary,
    WaitForTraceOptions,
};

fn traces(op: &str) -> Error {
    Error::unsupported(format!(
        "{op}: the OpenTelemetry Collector has no trace query API"
    ))
}

fn logs(op: &str) -> Error {
    Error::unsupported(format!(
        "{op}: the OpenTelemetry Collector has no log query API"
    ))
}

/// [`TraceQueryClient`] of the collector: every method returns `UNSUPPORTED`.
#[derive(Debug, Clone, Copy, Default)]
pub struct CollectorTraceQueryClient;

#[async_trait]
impl TraceQueryClient for CollectorTraceQueryClient {
    /// `UNSUPPORTED`.
    async fn get_trace(&self, _trace_id: &str) -> Result<Option<Trace>> {
        Err(traces("traces.getTrace"))
    }
    /// `UNSUPPORTED`.
    async fn search_traces(&self, _query: &TraceSearchQuery) -> Result<Vec<TraceSummary>> {
        Err(traces("traces.searchTraces"))
    }
    /// `UNSUPPORTED`.
    async fn list_services(&self) -> Result<Vec<String>> {
        Err(traces("traces.listServices"))
    }
    /// `UNSUPPORTED`.
    async fn list_operations(&self, _service: &str) -> Result<Vec<String>> {
        Err(traces("traces.listOperations"))
    }
    /// `UNSUPPORTED`.
    async fn get_dependencies(&self, _opts: DependencyOptions) -> Result<Vec<DependencyLink>> {
        Err(traces("traces.getDependencies"))
    }
    /// `UNSUPPORTED`.
    async fn wait_for_trace(&self, _trace_id: &str, _opts: WaitForTraceOptions) -> Result<Trace> {
        Err(traces("traces.waitForTrace"))
    }
}

/// [`LogQueryClient`] of the collector: every method returns `UNSUPPORTED`.
#[derive(Debug, Clone, Copy, Default)]
pub struct CollectorLogQueryClient;

#[async_trait]
impl LogQueryClient for CollectorLogQueryClient {
    /// `UNSUPPORTED`.
    async fn search(&self, _query: &str, _opts: SearchOptions) -> Result<LogSearchResult> {
        Err(logs("logs.search"))
    }
    /// `UNSUPPORTED`.
    async fn tail(
        &self,
        _query: &str,
        _on_record: RecordCallback,
        _opts: TailOptions,
    ) -> Result<TailHandle> {
        Err(logs("logs.tail"))
    }
    /// `UNSUPPORTED`.
    async fn list_streams(&self) -> Result<Vec<String>> {
        Err(logs("logs.listStreams"))
    }
    /// `UNSUPPORTED`.
    async fn list_labels(&self) -> Result<Vec<String>> {
        Err(logs("logs.listLabels"))
    }
    /// `UNSUPPORTED`.
    async fn wait_for_log(&self, _query: &str, _opts: WaitOptions) -> Result<LogRecord> {
        Err(logs("logs.waitForLog"))
    }
}
