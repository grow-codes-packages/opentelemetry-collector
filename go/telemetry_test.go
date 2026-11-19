package otelcollector

import (
	"context"
	"errors"
	"net/http"
	"sync"
	"testing"
	"time"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/attribute"
	otellog "go.opentelemetry.io/otel/log"
	sdklog "go.opentelemetry.io/otel/sdk/log"
	sdkmetric "go.opentelemetry.io/otel/sdk/metric"
	"go.opentelemetry.io/otel/sdk/metric/metricdata"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.opentelemetry.io/otel/sdk/trace/tracetest"
)

// otlpHTTPSink counts OTLP/HTTP requests per path.
type otlpHTTPSink struct {
	*testServer
	mu    sync.Mutex
	paths map[string]int
}

func newOTLPHTTPSink(t *testing.T) *otlpHTTPSink {
	s := &otlpHTTPSink{paths: map[string]int{}}
	s.testServer = newTestServer(t, func(w http.ResponseWriter, r *http.Request, _ int) {
		_ = readBody(t, r)
		s.mu.Lock()
		s.paths[r.URL.Path]++
		s.mu.Unlock()
		w.Header().Set("Content-Type", "application/x-protobuf")
		_, _ = w.Write([]byte{0x0a, 0x00})
	})
	return s
}

func (s *otlpHTTPSink) count(path string) int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.paths[path]
}

func TestInitTelemetryHTTP(t *testing.T) {
	sink := newOTLPHTTPSink(t)
	cfg := Config{
		CommonConfig:      CommonConfig{URL: sink.URL},
		ServiceName:       "telemetry-svc",
		Environment:       "test",
		DisableSetGlobal:  true,
		RuntimeMetrics:    true,
		ResourceDetectors: []string{},
		Compression:       CompressionNone,
	}
	h, err := InitTelemetry(t.Context(), cfg)
	if err != nil {
		t.Fatal(err)
	}
	_, span := h.Tracer("test").Start(t.Context(), "op")
	span.End()
	counter, err := h.Counter("requests", InstrumentOptions{Description: "d", Unit: "1"})
	if err != nil {
		t.Fatal(err)
	}
	counter.Add(t.Context(), 1)
	h.Logger("l", nil).Info(t.Context(), "log via LogClient")
	rec := otellog.Record{}
	rec.SetBody(attribute.StringValue("log via LoggerProvider"))
	h.LoggerProvider().Logger("bridge").Emit(t.Context(), rec)
	if h.LogClient() == nil || h.Tracing() == nil || h.Config().ServiceName != "telemetry-svc" {
		t.Fatal("accessors")
	}
	if v, ok := h.Resource().Set().Value("deployment.environment.name"); !ok || v.AsString() != "test" {
		t.Fatalf("resource: %v", h.Resource())
	}
	if err := h.ForceFlush(t.Context()); err != nil {
		t.Fatal(err)
	}
	if sink.count("/v1/traces") != 1 || sink.count("/v1/metrics") != 1 || sink.count("/v1/logs") != 2 {
		t.Fatalf("paths: %v", sink.paths)
	}
	if err := h.Shutdown(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err := h.Shutdown(t.Context()); err != nil {
		t.Fatal("second shutdown")
	}
}

func TestInitTelemetryGRPC(t *testing.T) {
	srv := newOTLPServer(t)
	c := newTestClient(t, "http://127.0.0.1:1", func(cfg *Config) {
		cfg.Protocol = ProtocolGRPC
		cfg.GRPCURL = srv.url()
		cfg.DisableSetGlobal = true
		cfg.ServiceName = "grpc-svc"
	})
	h, err := c.InitTelemetry(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	_, span := h.Tracer("t").Start(t.Context(), "grpc-op")
	span.End()
	g, err := h.Gauge("temperature", InstrumentOptions{})
	if err != nil {
		t.Fatal(err)
	}
	g.Record(t.Context(), 42.5)
	h.Logger("l", nil).Warn(t.Context(), "grpc log")
	if err := h.ForceFlush(t.Context()); err != nil {
		t.Fatal(err)
	}
	traces, metrics, logs, _ := srv.snapshot()
	if traces != 1 || metrics != 1 || len(logs) != 1 {
		t.Fatalf("server: traces=%d metrics=%d logs=%d", traces, metrics, len(logs))
	}
	if err := c.Close(); err != nil {
		t.Fatal(err)
	}
	if err := h.Shutdown(t.Context()); err != nil {
		t.Fatal("already shut down by Close")
	}
	if _, err := c.InitTelemetry(t.Context()); !errors.Is(err, ErrCancelled) {
		t.Fatalf("after close: %v", err)
	}
}

func TestTelemetryMetricsHelpers(t *testing.T) {
	reader := sdkmetric.NewManualReader()
	spans := tracetest.NewInMemoryExporter()
	logs := &memoryOTelLogs{}
	clientLogs := &memoryLogExporter{}
	h, err := InitTelemetry(t.Context(), Config{ServiceName: "m-svc", ResourceDetectors: []string{DetectorHost}},
		WithMetricReader(reader),
		WithTraceExporterFactory(func(context.Context, TracingConfig) (sdktrace.SpanExporter, error) { return spans, nil }),
		WithOTelLogExporter(logs),
		WithLogClientExporter(clientLogs),
	)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = h.Shutdown(context.Background()) }()
	if otel.GetMeterProvider() != h.MeterProvider() || otel.GetLoggerProvider() != h.LoggerProvider() {
		t.Fatal("globals not set")
	}
	ctx := t.Context()
	c, _ := h.Counter("grow_requests", InstrumentOptions{})
	c.Add(ctx, 3)
	hist, err := h.Histogram("grow_latency", InstrumentOptions{Unit: "ms", Boundaries: []float64{10, 100}})
	if err != nil {
		t.Fatal(err)
	}
	for _, v := range []float64{5, 50, 95} {
		hist.Record(ctx, v)
	}
	g, _ := h.Gauge("grow_temperature", InstrumentOptions{})
	g.Record(ctx, 42.5)
	if _, err := Meter("").Int64Counter("x"); err != nil {
		t.Fatal(err)
	}
	if _, err := h.Meter("").Int64Counter("y"); err != nil {
		t.Fatal(err)
	}
	ud, err := h.UpDownCounter("grow_inflight", InstrumentOptions{Meter: "custom"})
	if err != nil {
		t.Fatal(err)
	}
	ud.Add(ctx, -2)
	gc, err1 := Counter("global_counter", InstrumentOptions{})
	_, err2 := UpDownCounter("global_updown", InstrumentOptions{})
	_, err3 := Histogram("global_hist", InstrumentOptions{Boundaries: []float64{1}})
	_, err4 := Gauge("global_gauge", InstrumentOptions{})
	if err := errors.Join(err1, err2, err3, err4); err != nil {
		t.Fatal(err)
	}
	gc.Add(ctx, 1)
	var rm metricdata.ResourceMetrics
	if err := reader.Collect(ctx, &rm); err != nil {
		t.Fatal(err)
	}
	found := map[string]bool{}
	for _, sm := range rm.ScopeMetrics {
		if sm.Scope.Name != DefaultMeterName {
			continue
		}
		for _, m := range sm.Metrics {
			found[m.Name] = true
			switch d := m.Data.(type) {
			case metricdata.Sum[int64]:
				if m.Name == "grow_requests" && (!d.IsMonotonic || d.Temporality != metricdata.CumulativeTemporality || d.DataPoints[0].Value != 3) {
					t.Fatalf("counter: %+v", d)
				}
			case metricdata.Histogram[float64]:
				dp := d.DataPoints[0]
				if dp.Count != 3 || dp.Sum != 150 || len(dp.BucketCounts) != 3 || dp.BucketCounts[0] != 1 || dp.BucketCounts[1] != 2 {
					t.Fatalf("histogram: %+v", dp)
				}
			case metricdata.Gauge[float64]:
				if d.DataPoints[0].Value != 42.5 {
					t.Fatalf("gauge: %+v", d)
				}
			}
		}
	}
	if !found["grow_requests"] || !found["grow_latency"] || !found["grow_temperature"] {
		t.Fatalf("metrics: %v", found)
	}
	if _, err := h.Counter("", InstrumentOptions{}); !errors.Is(err, ErrValidation) {
		t.Fatalf("invalid counter: %v", err)
	}
	if _, err := h.Histogram("", InstrumentOptions{}); !errors.Is(err, ErrValidation) {
		t.Fatalf("invalid histogram: %v", err)
	}
	if _, err := h.Gauge("", InstrumentOptions{}); !errors.Is(err, ErrValidation) {
		t.Fatalf("invalid gauge: %v", err)
	}
	if _, err := h.UpDownCounter("", InstrumentOptions{}); !errors.Is(err, ErrValidation) {
		t.Fatalf("invalid up/down counter: %v", err)
	}

	_, span := h.Tracer("t").Start(ctx, "s")
	span.SetAttributes(attribute.String("k", "v"))
	span.End()
	h.Logger("l", nil).Error(ctx, "client log")
	rec := otellog.Record{}
	rec.SetBody(attribute.StringValue("bridge log"))
	h.LoggerProvider().Logger("b").Emit(ctx, rec)
	if err := h.ForceFlush(ctx); err != nil {
		t.Fatal(err)
	}
	if len(spans.GetSpans()) != 1 || logs.count() != 1 || clientLogs.count() != 1 {
		t.Fatalf("spans=%d otel logs=%d client logs=%d", len(spans.GetSpans()), logs.count(), clientLogs.count())
	}
}

func TestTelemetryDisabledAndErrors(t *testing.T) {
	h, err := InitTelemetry(t.Context(), Config{DisableTraces: true, DisableLogs: true, DisableMetrics: true, DisableSetGlobal: true})
	if err != nil {
		t.Fatal(err)
	}
	if h.Tracing() != nil || h.LogClient() != nil {
		t.Fatal("disabled signals")
	}
	_, span := h.Tracer("x").Start(t.Context(), "noop")
	span.End()
	if span.SpanContext().IsValid() {
		t.Fatal("noop tracer")
	}
	h.Logger("x", nil).Info(t.Context(), "dropped")
	h.LoggerProvider().Logger("x").Emit(t.Context(), otellog.Record{})
	if c, err := h.Counter("c", InstrumentOptions{}); err != nil || c == nil {
		t.Fatal("noop counter")
	}
	nl := noopLogClient{}
	nl.Send(t.Context(), LogInput{})
	nl.SendBatch(t.Context(), nil)
	if nl.Flush(t.Context()) != nil || nl.Close(t.Context()) != nil || nl.Stats() != (BatchStats{}) || nl.Logger("n", nil) == nil {
		t.Fatal("noop log client")
	}
	if err := h.ForceFlush(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err := h.Shutdown(t.Context()); err != nil {
		t.Fatal(err)
	}

	if _, err := InitTelemetry(t.Context(), Config{Protocol: ProtocolNative}); !errors.Is(err, ErrConfig) {
		t.Fatalf("native: %v", err)
	}
	boom := ConfigError("factory failed")
	if _, err := InitTelemetry(t.Context(), Config{DisableSetGlobal: true},
		WithTraceExporterFactory(func(context.Context, TracingConfig) (sdktrace.SpanExporter, error) { return nil, boom })); !errors.Is(err, ErrConfig) {
		t.Fatalf("factory: %v", err)
	}
	if _, err := InitTelemetry(t.Context(), Config{DisableSetGlobal: true, DisableTraces: true, CommonConfig: CommonConfig{URL: "https://h:4318", TLS: TLSConfig{CACertPath: "/does/not/exist"}}}); !errors.Is(err, ErrConfig) {
		t.Fatalf("metric tls: %v", err)
	}
	if _, err := InitTelemetry(t.Context(), Config{DisableSetGlobal: true, DisableTraces: true, DisableMetrics: true, CommonConfig: CommonConfig{URL: "https://h:4318", TLS: TLSConfig{CACertPath: "/does/not/exist"}}}); !errors.Is(err, ErrConfig) {
		t.Fatalf("log tls: %v", err)
	}
	if _, err := InitTelemetry(t.Context(), Config{DisableSetGlobal: true, DisableTraces: true, DisableMetrics: true}, WithOTelLogExporter(&memoryOTelLogs{}), WithLogClientExporter(nil)); err != nil {
		t.Fatalf("defaults: %v", err)
	}

	t.Setenv(EnvPrefix+"SERVICE_NAME", "env-svc")
	t.Setenv(EnvPrefix+"TRACES", "false")
	t.Setenv(EnvPrefix+"METRICS", "false")
	t.Setenv(EnvPrefix+"LOGS", "false")
	h, err = InitTelemetryFromEnv(t.Context(), Config{DisableSetGlobal: true})
	if err != nil || h.Config().ServiceName != "env-svc" || h.Tracing() != nil {
		t.Fatalf("from env: %v", err)
	}
	t.Setenv(EnvPrefix+"PROTOCOL", "carrier-pigeon")
	if _, err := InitTelemetryFromEnv(t.Context(), Config{}); !errors.Is(err, ErrConfig) {
		t.Fatalf("bad env: %v", err)
	}
	if wrapTelemetryErr("x", context.Canceled) == nil || wrapTelemetryErr("x", ValidationError("x", "y")) == nil {
		t.Fatal("wrap")
	}
}

func TestTelemetryHTTPS(t *testing.T) {
	// https endpoints build TLS exporters without connecting.
	cfg := Config{CommonConfig: CommonConfig{URL: "https://collector.example:4318"}, DisableSetGlobal: true, ResourceDetectors: []string{}}
	h, err := InitTelemetry(t.Context(), cfg)
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Millisecond)
	defer cancel()
	_ = h.Shutdown(ctx)
	cfg.Protocol = ProtocolGRPC
	cfg.GRPCURL = "https://collector.example:4317"
	if h, err = InitTelemetry(t.Context(), cfg); err != nil {
		t.Fatal(err)
	}
	_ = h.Shutdown(ctx)
	e, err := NewOTLPLogExporter(cfg)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := e.exporter(t.Context(), true); err != nil {
		t.Fatal(err)
	}
	_ = e.Shutdown(ctx)
}

// memoryOTelLogs is an sdklog.Exporter that counts records.
type memoryOTelLogs struct {
	mu sync.Mutex
	n  int
}

func (m *memoryOTelLogs) Export(_ context.Context, recs []sdklog.Record) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.n += len(recs)
	return nil
}
func (m *memoryOTelLogs) Shutdown(context.Context) error   { return nil }
func (m *memoryOTelLogs) ForceFlush(context.Context) error { return nil }
func (m *memoryOTelLogs) count() int {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.n
}

// memoryLogExporter is a LogExporter that counts records.
type memoryLogExporter struct {
	mu sync.Mutex
	n  int
}

func (m *memoryLogExporter) Export(_ context.Context, recs []LogRecord, _ ExportOptions) (ExportResult, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.n += len(recs)
	return ExportResult{Accepted: len(recs)}, nil
}
func (m *memoryLogExporter) Shutdown(context.Context) error { return nil }
func (m *memoryLogExporter) count() int {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.n
}
