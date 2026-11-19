package integration

// Integration tests against the real OpenTelemetry Collector from docker/compose.yml.
// Every public function of the package is called at least once with a round-trip
// assertion: telemetry is sent to the collector and read back from the capture files
// (file exporter), the Jaeger sidecar or the collector's own endpoints.

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"log/slog"
	"os"
	"strings"
	"testing"
	"time"

	"go.opentelemetry.io/otel/attribute"
	otellog "go.opentelemetry.io/otel/log"

	otelcollector "github.com/grow-codes-packages/opentelemetry-collector/go"
)

// setup loads /bootstrap/env and returns a client from the environment.
func setup(t *testing.T) *otelcollector.Client {
	t.Helper()
	bootstrap := os.Getenv("GROW_BOOTSTRAP_ENV")
	if bootstrap == "" {
		bootstrap = "/bootstrap/env"
	}
	loadBootstrapEnv(t, bootstrap)
	if os.Getenv("GROW_OPENTELEMETRY_COLLECTOR_URL") == "" {
		t.Skip("GROW_OPENTELEMETRY_COLLECTOR_URL is not set; run through docker/compose.yml")
	}
	c, err := otelcollector.NewClientFromEnv()
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = c.Close() })
	return c
}

func runID(t *testing.T) string {
	t.Helper()
	b := make([]byte, 4)
	if _, err := rand.Read(b); err != nil {
		t.Fatal(err)
	}
	return "go-" + hex.EncodeToString(b)
}

func envConfig(t *testing.T, explicit otelcollector.Config) otelcollector.Config {
	t.Helper()
	cfg, err := otelcollector.ConfigFromEnvWith(explicit, nil)
	if err != nil {
		t.Fatal(err)
	}
	return cfg
}

// waitCapture polls a capture file until it contains every needle.
func waitCapture(t *testing.T, signal string, needles ...string) []byte {
	t.Helper()
	var last []byte
	_, err := otelcollector.PollUntil(context.Background(), "capture", 30*time.Second, 250*time.Millisecond,
		func(ctx context.Context) (bool, bool, error) {
			lines, err := captureLines(ctx, signal)
			if err != nil {
				return false, false, err
			}
			last = bytes.Join(lines, []byte("\n"))
			for _, n := range needles {
				if !bytes.Contains(last, []byte(n)) {
					return false, false, nil
				}
			}
			return true, true, nil
		})
	if err != nil {
		t.Fatalf("capture %s never contained %q: %v", signal, needles, err)
	}
	return last
}

// waitLogs waits until records with body containing marker exist and returns them.
func waitLogs(t *testing.T, marker string, minCount int) []otelcollector.LogRecord {
	t.Helper()
	recs, err := otelcollector.WaitForLogs(context.Background(), captureLogQuery{}, marker, minCount,
		otelcollector.WaitForLogOptions{Timeout: 30 * time.Second, Interval: 250 * time.Millisecond, Search: otelcollector.SearchOptions{Limit: 10000}})
	if err != nil {
		t.Fatalf("logs %q: %v", marker, err)
	}
	return recs
}

func waitTrace(t *testing.T, traceID string, minSpans int) *otelcollector.Trace {
	t.Helper()
	tr, err := jaegerTraceQuery{}.WaitForTrace(context.Background(), traceID,
		otelcollector.WaitForTraceOptions{Timeout: 30 * time.Second, Interval: 250 * time.Millisecond, MinSpans: minSpans})
	if err != nil {
		t.Fatalf("trace %s: %v", traceID, err)
	}
	return tr
}

func TestClientAgainstCollector(t *testing.T) {
	c := setup(t)
	ctx := t.Context()

	hs, err := c.Health(ctx)
	if err != nil || !hs.Healthy || hs.Status != "ok" || hs.Details["vendorStatus"] != "Server available" || hs.Details["upSince"] == "" {
		t.Fatalf("Health: %+v %v", hs, err)
	}
	if hs, err := c.WaitUntilReady(ctx, 10*time.Second); err != nil || !hs.Healthy {
		t.Fatalf("WaitUntilReady: %+v %v", hs, err)
	}
	if hs, err := c.CollectorHealth().Health(ctx); err != nil || !hs.Healthy {
		t.Fatalf("CollectorHealth: %+v %v", hs, err)
	}
	if d, err := c.Ping(ctx); err != nil || d <= 0 {
		t.Fatalf("Ping: %v %v", d, err)
	}
	want := os.Getenv("OTELCOL_TEST_SERVER_VERSION")
	if v, err := c.ServerVersion(ctx); err != nil || v != want || want == "" {
		t.Fatalf("ServerVersion: %q (want %q) %v", v, want, err)
	}
	// target_info fallback: zpages pointed at a path without the page.
	fb, err := otelcollector.NewClient(envConfig(t, otelcollector.Config{ZPagesURL: os.Getenv("OTELCOL_TEST_JAEGER_URL") + "/nope"}))
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = fb.Close() }()
	if v, err := fb.ServerVersion(ctx); err != nil || v != want {
		t.Fatalf("ServerVersion via target_info: %q %v", v, err)
	}
	// gRPC ping.
	g, err := otelcollector.NewClient(envConfig(t, otelcollector.Config{Protocol: otelcollector.ProtocolGRPC}))
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = g.Close() }()
	if _, err := g.Ping(ctx); err != nil {
		t.Fatalf("gRPC Ping: %v", err)
	}
	// Query clients: the collector has none.
	if _, err := c.TraceQuery().GetTrace(ctx, os.Getenv("OTELCOL_TEST_BOOTSTRAP_TRACE_ID")); !errors.Is(err, otelcollector.ErrUnsupported) {
		t.Fatalf("TraceQuery: %v", err)
	}
	if _, err := c.LogQuery().Search(ctx, "x", otelcollector.SearchOptions{}); !errors.Is(err, otelcollector.ErrUnsupported) {
		t.Fatalf("LogQuery: %v", err)
	}
	if err := c.Close(); err != nil {
		t.Fatal(err)
	}
	if err := c.Close(); err != nil {
		t.Fatal("second close")
	}
}

func TestAuthAndErrors(t *testing.T) {
	setup(t)
	ctx := t.Context()
	token := os.Getenv("OTELCOL_TEST_BEARER_TOKEN")
	authURL := os.Getenv("OTELCOL_TEST_AUTH_URL")
	authGRPC := os.Getenv("OTELCOL_TEST_AUTH_GRPC_URL")

	noAuth, err := otelcollector.NewClient(otelcollector.Config{CommonConfig: otelcollector.CommonConfig{URL: authURL}})
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = noAuth.Close() }()
	_, err = noAuth.Ping(ctx)
	if !errors.Is(err, otelcollector.ErrAuthentication) || !strings.Contains(err.Error(), "authorization") {
		t.Fatalf("Ping without token: %v", err)
	}
	wrongTok, _ := otelcollector.NewClient(otelcollector.Config{CommonConfig: otelcollector.CommonConfig{URL: authURL, Headers: map[string]string{"Authorization": "Bearer wrong-" + token}}})
	defer func() { _ = wrongTok.Close() }()
	if _, err := wrongTok.Ping(ctx); !errors.Is(err, otelcollector.ErrAuthentication) || strings.Contains(err.Error(), token) {
		t.Fatalf("Ping with a wrong token: %v", err)
	}
	headers := map[string]string{"Authorization": "Bearer " + token}
	withAuth, err := otelcollector.NewClient(otelcollector.Config{CommonConfig: otelcollector.CommonConfig{URL: authURL, Headers: headers}})
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = withAuth.Close() }()
	if _, err := withAuth.Ping(ctx); err != nil {
		t.Fatalf("Ping with token: %v", err)
	}
	grpcNoAuth, _ := otelcollector.NewClient(otelcollector.Config{Protocol: otelcollector.ProtocolGRPC, GRPCURL: authGRPC})
	defer func() { _ = grpcNoAuth.Close() }()
	if _, err := grpcNoAuth.Ping(ctx); !errors.Is(err, otelcollector.ErrAuthentication) {
		t.Fatalf("gRPC Ping without token: %v", err)
	}
	grpcAuth, _ := otelcollector.NewClient(otelcollector.Config{Protocol: otelcollector.ProtocolGRPC, GRPCURL: authGRPC, CommonConfig: otelcollector.CommonConfig{Headers: headers}})
	defer func() { _ = grpcAuth.Close() }()
	if _, err := grpcAuth.Ping(ctx); err != nil {
		t.Fatalf("gRPC Ping with token: %v", err)
	}

	// Log exporter: 401 without the token, 400 for a body above the 64 KiB limit.
	exp, err := noAuth.LogExporter()
	if err != nil {
		t.Fatal(err)
	}
	if _, err := exp.Export(ctx, []otelcollector.LogRecord{{Body: "x"}}, otelcollector.ExportOptions{}); !errors.Is(err, otelcollector.ErrAuthentication) {
		t.Fatalf("export without token: %v", err)
	}
	big, err := withAuth.LogExporter()
	if err != nil {
		t.Fatal(err)
	}
	if _, err := big.Export(ctx, []otelcollector.LogRecord{{Body: strings.Repeat("x", 100_000)}}, otelcollector.ExportOptions{}); !errors.Is(err, otelcollector.ErrValidation) {
		t.Fatalf("oversized export: %v", err)
	}
	// Wrong base path → NOT_FOUND.
	wrong, _ := otelcollector.NewClient(envConfig(t, otelcollector.Config{TracesURL: os.Getenv("GROW_OPENTELEMETRY_COLLECTOR_URL") + "/v1/nope"}))
	defer func() { _ = wrong.Close() }()
	if _, err := wrong.Ping(ctx); !errors.Is(err, otelcollector.ErrNotFound) {
		t.Fatalf("wrong path: %v", err)
	}
}

func TestStockCollector(t *testing.T) {
	setup(t)
	ctx := t.Context()
	stock, err := otelcollector.NewClient(otelcollector.Config{CommonConfig: otelcollector.CommonConfig{URL: os.Getenv("OTELCOL_TEST_STOCK_URL")}})
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = stock.Close() }()
	// health_check binds localhost on stock stacks → zpages fallback.
	hs, err := stock.Health(ctx)
	if err != nil || !hs.Healthy || hs.Details["source"] != "zpages" {
		t.Fatalf("stock Health: %+v %v", hs, err)
	}
	if v, err := stock.ServerVersion(ctx); err != nil || v != os.Getenv("OTELCOL_TEST_SERVER_VERSION") {
		t.Fatalf("stock ServerVersion: %q %v", v, err)
	}
	if _, err := stock.Ping(ctx); err != nil {
		t.Fatalf("stock Ping: %v", err)
	}
	h, err := otelcollector.NewCollectorHealthClient(otelcollector.Config{
		CommonConfig: otelcollector.CommonConfig{URL: os.Getenv("OTELCOL_TEST_STOCK_URL")},
		HealthURL:    os.Getenv("OTELCOL_TEST_STOCK_HEALTH_URL"),
	})
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = h.Close() }()
	if hs, err := h.WaitUntilReady(ctx, 5*time.Second); err != nil || hs.Details["source"] != "zpages" {
		t.Fatalf("stock WaitUntilReady: %+v %v", hs, err)
	}
	lc, err := stock.LogClient(otelcollector.LogClientOptions{})
	if err != nil {
		t.Fatal(err)
	}
	lc.Send(ctx, otelcollector.LogInput{Message: "stock " + runID(t)})
	if err := lc.Flush(ctx); err != nil || lc.Stats().Sent != 1 {
		t.Fatalf("stock export: %v %+v", err, lc.Stats())
	}
}

func TestLogExporterAndLogClient(t *testing.T) {
	c := setup(t)
	ctx := t.Context()
	run := runID(t)

	for _, protocol := range []otelcollector.Protocol{otelcollector.ProtocolHTTPProtobuf, otelcollector.ProtocolGRPC} {
		cl, err := otelcollector.NewClient(envConfig(t, otelcollector.Config{Protocol: protocol}))
		if err != nil {
			t.Fatal(err)
		}
		exp, err := cl.LogExporter()
		if err != nil {
			t.Fatal(err)
		}
		flags := uint8(1)
		marker := "exporter " + string(protocol) + " " + run
		res, err := exp.Export(ctx, []otelcollector.LogRecord{{
			Timestamp: time.Now(), Severity: otelcollector.SeverityError, SeverityText: "ERROR", Body: marker,
			Attributes: map[string]otelcollector.AttrValue{"grow.run": run, "user.id": int64(42), "nested": map[string]otelcollector.AttrValue{"k": "v"}},
			Resource:   map[string]otelcollector.AttrValue{"service.name": "go-exporter-" + run},
			TraceID:    "4bf92f3577b34da6a3ce929d0e0e4736", SpanID: "00f067aa0ba902b7", TraceFlags: &flags,
		}}, otelcollector.ExportOptions{Gzip: true})
		if err != nil || res.Accepted != 1 || res.Rejected != 0 {
			t.Fatalf("%s Export: %+v %v", protocol, res, err)
		}
		r := waitLogs(t, marker, 1)[0]
		if r.Severity != otelcollector.SeverityError || r.TraceID != "4bf92f3577b34da6a3ce929d0e0e4736" || r.SpanID != "00f067aa0ba902b7" ||
			r.Attributes["user.id"] != int64(42) || r.Resource["service.name"] != "go-exporter-"+run {
			t.Fatalf("%s readback: %+v", protocol, r)
		}
		if err := exp.Shutdown(ctx); err != nil {
			t.Fatal(err)
		}
		_ = cl.Close()
	}

	// LogClient + Logger + slog adapter.
	lc, err := c.LogClient(otelcollector.LogClientOptions{})
	if err != nil {
		t.Fatal(err)
	}
	lc.Logger("checkout", map[string]otelcollector.AttrValue{"component": "cart"}).Warn(ctx, "logger "+run, "password", "hunter2-"+run)
	slog.New(otelcollector.NewSlogHandler(lc, nil)).InfoContext(ctx, "slog "+run, "n", 7)
	lc.SendBatch(ctx, []otelcollector.LogEntry{otelcollector.LogInput{Message: "batch a " + run}, otelcollector.LogInput{Message: "batch b " + run}})
	if err := lc.Flush(ctx); err != nil {
		t.Fatal(err)
	}
	recs := waitLogs(t, run, 6) // 2 exporter + logger + slog + 2 batch
	var logger *otelcollector.LogRecord
	for i := range recs {
		if recs[i].Body == "logger "+run {
			logger = &recs[i]
		}
	}
	if logger == nil || logger.Attributes["password"] != "[REDACTED]" || logger.Attributes["component"] != "cart" ||
		logger.Resource["service.name"] != c.Config().ServiceName {
		t.Fatalf("logger record: %+v", logger)
	}
	if st := lc.Stats(); st.Sent != 4 || st.Dropped != 0 {
		t.Fatalf("stats: %+v", st)
	}
	if err := lc.Close(ctx); err != nil {
		t.Fatal(err)
	}
	// Package-level NewLogClient.
	plc, err := otelcollector.NewLogClient(c.Config(), otelcollector.LogClientOptions{})
	if err != nil {
		t.Fatal(err)
	}
	plc.Send(ctx, otelcollector.LogInput{Message: "package client " + run})
	_ = plc.Close(ctx)
	waitLogs(t, "package client "+run, 1)
}

func TestInitTelemetryRoundTrip(t *testing.T) {
	c := setup(t)
	for _, protocol := range []otelcollector.Protocol{otelcollector.ProtocolHTTPProtobuf, otelcollector.ProtocolGRPC} {
		t.Run(string(protocol), func(t *testing.T) {
			ctx := t.Context()
			run := runID(t)
			service := "go-telemetry-" + strings.ReplaceAll(string(protocol), "/", "-") + "-" + run
			cfg := envConfig(t, otelcollector.Config{
				Protocol: protocol, ServiceName: service, ServiceVersion: "1.2.3", Environment: "sdk-test",
				ResourceAttributes: map[string]any{"grow.run": run}, DisableSetGlobal: true, RuntimeMetrics: true,
			})
			var h *otelcollector.TelemetryHandle
			var err error
			if protocol == otelcollector.ProtocolGRPC {
				h, err = otelcollector.InitTelemetry(ctx, cfg)
			} else {
				h, err = c.InitTelemetry(ctx) // client config + env
				if err == nil {
					_ = h.Shutdown(ctx)
				}
				h, err = otelcollector.InitTelemetryFromEnv(ctx, otelcollector.Config{
					Protocol: protocol, ServiceName: service, ServiceVersion: "1.2.3", Environment: "sdk-test",
					ResourceAttributes: map[string]any{"grow.run": run}, DisableSetGlobal: true, RuntimeMetrics: true,
				})
			}
			if err != nil {
				t.Fatal(err)
			}
			defer func() { _ = h.Shutdown(context.Background()) }()

			// traces → Jaeger
			tctx, root := h.Tracer("grow-it").Start(ctx, "root-"+run)
			_, child := h.Tracer("grow-it").Start(tctx, "child")
			child.End()
			root.End()
			traceID := root.SpanContext().TraceID().String()
			// metrics → capture
			counter, err := h.Counter("grow_it_requests", otelcollector.InstrumentOptions{Unit: "1"})
			if err != nil {
				t.Fatal(err)
			}
			counter.Add(ctx, 3)
			hist, _ := h.Histogram("grow_it_latency", otelcollector.InstrumentOptions{Boundaries: []float64{10, 100}})
			hist.Record(ctx, 50)
			gauge, _ := h.Gauge("grow_it_temperature", otelcollector.InstrumentOptions{})
			gauge.Record(ctx, 42.5)
			mc, _ := h.Meter("custom").Int64Counter("grow_it_custom")
			mc.Add(ctx, 1)
			// logs → capture (LogClient and the OpenTelemetry LoggerProvider)
			h.Logger("it", nil).Info(ctx, "telemetry logclient "+run)
			var rec otellog.Record
			rec.SetBody(attribute.StringValue("telemetry bridge " + run))
			rec.SetSeverity(otellog.SeverityWarn)
			h.LoggerProvider().Logger("bridge").Emit(tctx, rec)
			h.LogClient().Send(ctx, otelcollector.LogInput{Message: "telemetry direct " + run})

			if err := h.ForceFlush(ctx); err != nil {
				t.Fatal(err)
			}
			tr := waitTrace(t, traceID, 2)
			if tr.Spans[0].ServiceName != service || len(tr.Tree()) != 1 {
				t.Fatalf("trace: %+v", tr)
			}
			metrics := waitCapture(t, "metrics", service, "grow_it_requests", "grow_it_latency", "grow_it_temperature", "grow_it_custom", "go.memory.used")
			if !bytes.Contains(metrics, []byte(`"explicitBounds":[10,100]`)) {
				t.Fatal("histogram bounds missing in capture")
			}
			recs := waitLogs(t, run, 3)
			var bridge *otelcollector.LogRecord
			for i := range recs {
				if recs[i].Body == "telemetry bridge "+run {
					bridge = &recs[i]
				}
			}
			if bridge == nil || bridge.TraceID != traceID || bridge.Resource["service.name"] != service ||
				bridge.Resource["deployment.environment.name"] != "sdk-test" || bridge.Resource["host.name"] == nil {
				t.Fatalf("bridge log: %+v", bridge)
			}
			if h.TracerProvider() == nil || h.MeterProvider() == nil || h.Tracing() == nil || h.Resource() == nil || h.Config().ServiceName != service {
				t.Fatal("accessors")
			}
			if err := h.Shutdown(ctx); err != nil {
				t.Fatal(err)
			}
		})
	}
}

func TestInitTracingRoundTrip(t *testing.T) {
	c := setup(t)
	ctx := t.Context()
	run := runID(t)

	// Package InitTracing from the environment.
	h, err := otelcollector.InitTracing(ctx, otelcollector.TracingConfig{ServiceName: "go-tracing-" + run, DisableSetGlobal: true})
	if err != nil {
		t.Fatal(err)
	}
	_, err = otelcollector.WithSpan(ctx, "with-span-"+run, func(ctx context.Context) (int, error) {
		otelcollector.AddEvent(ctx, "checkpoint", map[string]any{"n": 1})
		return 1, nil
	}, otelcollector.WithTracer(h.Tracer("it")))
	if err != nil {
		t.Fatal(err)
	}
	sctx, span := h.Tracer("it").Start(ctx, "traced-"+run)
	traceID, _ := otelcollector.CurrentTraceID(sctx)
	span.End()
	if err := h.ForceFlush(ctx); err != nil {
		t.Fatal(err)
	}
	if tr := waitTrace(t, traceID, 1); tr.Spans[0].ServiceName != "go-tracing-"+run {
		t.Fatalf("trace: %+v", tr)
	}
	_ = h.Shutdown(ctx)

	// Client.InitTracing over gRPC, TracingConfigFromEnv + NewExporter.
	tc, err := otelcollector.TracingConfigFromEnv(otelcollector.TracingConfig{ServiceName: "go-grpc-" + run, Protocol: otelcollector.ProtocolGRPC}, nil)
	if err != nil || !strings.HasSuffix(tc.Endpoint, ":4317") {
		t.Fatalf("TracingConfigFromEnv: %s %v", tc, err)
	}
	gh, err := c.InitTracing(ctx, otelcollector.TracingConfig{ServiceName: "go-grpc-" + run, Protocol: otelcollector.ProtocolGRPC, DisableSetGlobal: true},
		otelcollector.WithExporterFactory(otelcollector.NewExporter))
	if err != nil {
		t.Fatal(err)
	}
	gctx, gspan := gh.Tracer("it").Start(ctx, "grpc-"+run)
	gid, _ := otelcollector.CurrentSpanID(gctx)
	gspan.End()
	if err := gh.ForceFlush(ctx); err != nil {
		t.Fatal(err)
	}
	tr := waitTrace(t, gspan.SpanContext().TraceID().String(), 1)
	if tr.Spans[0].SpanID != gid || tr.Spans[0].ServiceName != "go-grpc-"+run {
		t.Fatalf("grpc trace: %+v", tr)
	}
	// DetectResource is the resource of every provider.
	res, err := otelcollector.DetectResource(ctx, c.Config())
	if err != nil || res.Len() == 0 {
		t.Fatalf("DetectResource: %v", err)
	}
}
