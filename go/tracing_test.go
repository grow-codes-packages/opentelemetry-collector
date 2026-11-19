package otelcollector

import (
	"context"
	"errors"
	"testing"
	"time"

	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.opentelemetry.io/otel/sdk/trace/tracetest"
)

func TestTracingConfigFromEnv(t *testing.T) {
	env := map[string]string{
		"OTEL_EXPORTER_OTLP_ENDPOINT":       "http://otel:4318",
		"OTEL_EXPORTER_OTLP_TRACES_HEADERS": "X-T=1",
		"OTEL_SERVICE_NAME":                 "env-svc",
		"OTEL_RESOURCE_ATTRIBUTES":          "team=core,k=env",
		"OTEL_TRACES_SAMPLER":               "traceidratio",
		"OTEL_TRACES_SAMPLER_ARG":           "0.5",
		"OTEL_BSP_SCHEDULE_DELAY":           "100",
	}
	tc, err := TracingConfigFromEnv(TracingConfig{ResourceAttributes: map[string]any{"k": "explicit"}, Headers: map[string]string{"X-E": "2"}}, env)
	if err != nil {
		t.Fatal(err)
	}
	if tc.Endpoint != "http://otel:4318/v1/traces" || tc.Protocol != ProtocolHTTPProtobuf || tc.ServiceName != "env-svc" ||
		tc.Headers["X-T"] != "1" || tc.Headers["X-E"] != "2" || tc.ResourceAttributes["team"] != "core" ||
		tc.ResourceAttributes["k"] != "explicit" || tc.Sampler != SamplerTraceIDRatio || tc.SamplerRatio != 0.5 ||
		tc.Batch.ScheduledDelay != 100*time.Millisecond || tc.Compression != CompressionGzip {
		t.Fatalf("resolved: %s %v", tc, tc.ResourceAttributes)
	}

	tc, err = TracingConfigFromEnv(TracingConfig{ServiceName: "x", Endpoint: "http://explicit:4318"}, nil)
	if err != nil || tc.Endpoint != "http://explicit:4318/v1/traces" || tc.ServiceName != "x" {
		t.Fatalf("explicit endpoint: %s %v", tc, err)
	}
	tc, err = TracingConfigFromEnv(TracingConfig{Protocol: ProtocolGRPC, Endpoint: "http://g:4317"}, map[string]string{})
	if err != nil || tc.Endpoint != "http://g:4317" {
		t.Fatalf("grpc explicit: %s %v", tc, err)
	}
	tc, err = TracingConfigFromEnv(TracingConfig{Protocol: ProtocolGRPC}, map[string]string{EnvPrefix + "GRPC_URL": "http://grow:4317"})
	if err != nil || tc.Endpoint != "http://grow:4317" || tc.Protocol != ProtocolGRPC {
		t.Fatalf("grpc env: %s %v", tc, err)
	}
	if _, err := TracingConfigFromEnv(TracingConfig{Protocol: ProtocolNative}, nil); !errors.Is(err, ErrConfig) {
		t.Fatalf("native: %v", err)
	}
	if _, err := TracingConfigFromEnv(TracingConfig{}, map[string]string{"OTEL_TRACES_SAMPLER": "nope"}); !errors.Is(err, ErrConfig) {
		t.Fatalf("bad env: %v", err)
	}

	base := Config{CommonConfig: CommonConfig{URL: "http://c:4318"}, GRPCURL: "http://c:4317", ServiceName: "base", Environment: "dev", ServiceVersion: "1"}
	if got := TracingConfigFrom(TracingConfig{Protocol: ProtocolGRPC}, base); got.Endpoint != "http://c:4317" {
		t.Fatalf("switch to grpc: %s", got)
	}
	base.Protocol = ProtocolGRPC
	got := TracingConfigFrom(TracingConfig{Protocol: ProtocolHTTPProtobuf, Batch: TracingBatchOptions{MaxQueueSize: 7}, TLS: TLSConfig{CACertPath: "/x"}}, base)
	if got.Endpoint != "http://c:4318/v1/traces" || got.Batch.MaxQueueSize != 7 || got.TLS.CACertPath != "/x" || got.Environment != "dev" {
		t.Fatalf("switch to http: %s", got)
	}
	if mergeStrings(nil, nil) != nil || mergeAny(nil, nil) != nil {
		t.Fatal("merge nil")
	}
}

func TestInitTracing(t *testing.T) {
	mem := tracetest.NewInMemoryExporter()
	factory := func(_ context.Context, cfg TracingConfig) (sdktrace.SpanExporter, error) {
		if cfg.Endpoint != "http://otel:4318/v1/traces" {
			t.Errorf("endpoint %q", cfg.Endpoint)
		}
		return mem, nil
	}
	h, err := InitTracing(t.Context(), TracingConfig{ServiceName: "trace-svc", DisableSetGlobal: true},
		WithEnv(map[string]string{"OTEL_EXPORTER_OTLP_ENDPOINT": "http://otel:4318"}),
		WithExporterFactory(factory), WithResourceDetectors())
	if err != nil {
		t.Fatal(err)
	}
	_, err = WithSpan(t.Context(), "op", func(context.Context) (int, error) { return 1, nil }, WithTracer(h.Tracer("t")))
	if err != nil {
		t.Fatal(err)
	}
	if err := h.ForceFlush(t.Context()); err != nil {
		t.Fatal(err)
	}
	spans := mem.GetSpans()
	if len(spans) != 1 {
		t.Fatalf("spans: %d", len(spans))
	}
	if v, ok := spans[0].Resource.Set().Value("service.name"); !ok || v.AsString() != "trace-svc" {
		t.Fatalf("resource: %v", spans[0].Resource)
	}
	_ = h.Shutdown(t.Context())

	if _, err := InitTracing(t.Context(), TracingConfig{Protocol: ProtocolNative}); !errors.Is(err, ErrConfig) {
		t.Fatalf("native: %v", err)
	}
	if _, err := InitTracing(t.Context(), TracingConfig{Sampler: "nope"}, WithEnv(map[string]string{})); !errors.Is(err, ErrConfig) {
		t.Fatalf("invalid: %v", err)
	}
	boom := ConfigError("no exporter")
	if _, err := InitTracing(t.Context(), TracingConfig{}, WithEnv(map[string]string{}),
		WithExporterFactory(func(context.Context, TracingConfig) (sdktrace.SpanExporter, error) { return nil, boom })); !errors.Is(err, ErrConfig) {
		t.Fatalf("factory: %v", err)
	}

	// The default factory builds the official OTLP exporter (no connection yet).
	h, err = InitTracing(t.Context(), TracingConfig{ServiceName: "default", DisableSetGlobal: true}, WithEnv(map[string]string{}))
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Millisecond)
	defer cancel()
	_ = h.Shutdown(ctx)
	exp, err := NewExporter(t.Context(), TracingConfig{ServiceName: "x"})
	if err != nil {
		t.Fatal(err)
	}
	_ = exp.Shutdown(ctx)
	if _, err := NewExporter(t.Context(), TracingConfig{Protocol: ProtocolNative}); !errors.Is(err, ErrConfig) {
		t.Fatalf("NewExporter native: %v", err)
	}
}

func TestClientInitTracing(t *testing.T) {
	sink := newOTLPHTTPSink(t)
	c := newTestClient(t, sink.URL, func(cfg *Config) { cfg.ServiceName = "client-svc"; cfg.DisableSetGlobal = true })
	h, err := c.InitTracing(t.Context(), TracingConfig{})
	if err != nil {
		t.Fatal(err)
	}
	_, span := h.Tracer("t").Start(t.Context(), "op")
	span.End()
	if err := h.ForceFlush(t.Context()); err != nil {
		t.Fatal(err)
	}
	if sink.count("/v1/traces") != 1 || h.Config().ServiceName != "client-svc" {
		t.Fatalf("paths %v", sink.paths)
	}
	if _, err := c.InitTracing(t.Context(), TracingConfig{Protocol: ProtocolNative}); !errors.Is(err, ErrConfig) {
		t.Fatalf("native: %v", err)
	}
	if _, err := c.InitTracing(t.Context(), TracingConfig{}, WithExporterFactory(func(context.Context, TracingConfig) (sdktrace.SpanExporter, error) {
		return nil, ConfigError("x")
	})); !errors.Is(err, ErrConfig) {
		t.Fatalf("factory: %v", err)
	}
	_ = c.Close()
	if _, err := c.InitTracing(t.Context(), TracingConfig{}); !errors.Is(err, ErrCancelled) {
		t.Fatalf("closed: %v", err)
	}
}
