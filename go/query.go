package otelcollector

import "context"

// UnsupportedTraceQueryClient implements TraceQueryClient for the collector, which
// stores nothing and has no query API: every method returns an UNSUPPORTED error.
// Read traces back from the backend the collector exports to (Jaeger, Tempo, …).
type UnsupportedTraceQueryClient struct{}

var _ TraceQueryClient = UnsupportedTraceQueryClient{}

func unsupportedTraceQuery(op string) error {
	return UnsupportedError(op, "the OpenTelemetry Collector has no trace query API")
}

// GetTrace returns UNSUPPORTED.
func (UnsupportedTraceQueryClient) GetTrace(context.Context, string) (*Trace, error) {
	return nil, unsupportedTraceQuery("traces.getTrace")
}

// SearchTraces returns UNSUPPORTED.
func (UnsupportedTraceQueryClient) SearchTraces(context.Context, TraceSearchQuery) ([]TraceSummary, error) {
	return nil, unsupportedTraceQuery("traces.searchTraces")
}

// ListServices returns UNSUPPORTED.
func (UnsupportedTraceQueryClient) ListServices(context.Context) ([]string, error) {
	return nil, unsupportedTraceQuery("traces.listServices")
}

// ListOperations returns UNSUPPORTED.
func (UnsupportedTraceQueryClient) ListOperations(context.Context, string) ([]string, error) {
	return nil, unsupportedTraceQuery("traces.listOperations")
}

// GetDependencies returns UNSUPPORTED.
func (UnsupportedTraceQueryClient) GetDependencies(context.Context, DependencyOptions) ([]DependencyLink, error) {
	return nil, unsupportedTraceQuery("traces.getDependencies")
}

// WaitForTrace returns UNSUPPORTED.
func (UnsupportedTraceQueryClient) WaitForTrace(context.Context, string, WaitForTraceOptions) (*Trace, error) {
	return nil, unsupportedTraceQuery("traces.waitForTrace")
}
