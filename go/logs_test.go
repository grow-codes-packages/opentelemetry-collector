package otelcollector

import (
	"context"
	"encoding/hex"
	"errors"
	"net/http"
	"strings"
	"sync"
	"testing"
	"time"

	"google.golang.org/grpc/codes"
	"google.golang.org/protobuf/proto"

	collogspb "go.opentelemetry.io/proto/otlp/collector/logs/v1"
	commonpb "go.opentelemetry.io/proto/otlp/common/v1"
)

// logSink is an OTLP/HTTP logs endpoint that decodes and keeps every request.
type logSink struct {
	*testServer
	mu   sync.Mutex
	reqs []*collogspb.ExportLogsServiceRequest
	gzip []bool
}

func newLogSink(t *testing.T, respond func(w http.ResponseWriter, n int) bool) *logSink {
	t.Helper()
	s := &logSink{}
	ok := loadFixture(t, "exportLogs.http_protobuf_ok")
	s.testServer = newTestServer(t, func(w http.ResponseWriter, r *http.Request, n int) {
		if r.URL.Path != "/v1/logs" || r.Header.Get("Content-Type") != "application/x-protobuf" {
			w.WriteHeader(http.StatusNotFound)
			return
		}
		body := readBody(t, r)
		var req collogspb.ExportLogsServiceRequest
		if err := proto.Unmarshal(body, &req); err != nil {
			t.Errorf("unmarshal: %v", err)
		}
		s.mu.Lock()
		s.reqs = append(s.reqs, &req)
		s.gzip = append(s.gzip, r.Header.Get("Content-Encoding") == "gzip")
		s.mu.Unlock()
		if respond != nil && respond(w, n) {
			return
		}
		ok.serve(w)
	})
	return s
}

func (s *logSink) requests() []*collogspb.ExportLogsServiceRequest {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]*collogspb.ExportLogsServiceRequest(nil), s.reqs...)
}

func attrMap(kvs []*commonpb.KeyValue) map[string]*commonpb.AnyValue {
	out := map[string]*commonpb.AnyValue{}
	for _, kv := range kvs {
		out[kv.Key] = kv.Value
	}
	return out
}

func testExporter(t *testing.T, url string, mutate func(*Config)) *OTLPLogExporter {
	t.Helper()
	cfg := Config{CommonConfig: CommonConfig{URL: url, Retry: fastRetry(0)}, ServiceName: "svc"}
	if mutate != nil {
		mutate(&cfg)
	}
	e, err := NewOTLPLogExporter(cfg)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = e.Shutdown(context.Background()) })
	return e
}

func TestOTLPLogExporterHTTP(t *testing.T) {
	sink := newLogSink(t, nil)
	e := testExporter(t, sink.URL, func(c *Config) { c.Headers = map[string]string{"Authorization": "Bearer " + testToken} })
	flags := uint8(1)
	ts := time.Date(2026, 11, 18, 12, 0, 0, 0, time.UTC)
	recs := []LogRecord{
		{
			Timestamp: ts, Severity: SeverityError, SeverityText: "ERROR", Body: "hello",
			Attributes: map[string]AttrValue{
				"n": int64(42), "ok": true, "f": 1.5, "list": []AttrValue{"a", int64(1)},
				"nested": map[string]AttrValue{"x": "y"}, "nil": nil, "weird": struct{ A int }{A: 1},
			},
			Resource: map[string]AttrValue{"service.name": "a"},
			TraceID:  "4bf92f3577b34da6a3ce929d0e0e4736", SpanID: "00f067aa0ba902b7", TraceFlags: &flags,
		},
		{Timestamp: ts, ObservedTimestamp: ts.Add(time.Second), Severity: 99, Body: "other resource", Resource: map[string]AttrValue{"service.name": "b"}, TraceID: "zz"},
		{Timestamp: ts, Severity: SeverityInfo, Body: "same as first", Resource: map[string]AttrValue{"service.name": "a"}},
	}
	res, err := e.Export(t.Context(), recs, ExportOptions{Gzip: true})
	if err != nil || res.Accepted != 3 || res.Rejected != 0 || res.Error != nil {
		t.Fatalf("export: %+v %v", res, err)
	}
	reqs := sink.requests()
	if len(reqs) != 2 || !sink.gzip[0] {
		t.Fatalf("requests: %d gzip=%v", len(reqs), sink.gzip)
	}
	first := reqs[0].ResourceLogs[0]
	if attrMap(first.Resource.Attributes)["service.name"].GetStringValue() != "a" {
		t.Fatalf("resource: %v", first.Resource)
	}
	sl := first.ScopeLogs[0]
	if sl.Scope.Name != InstrumentationScope || sl.Scope.Version != Version || len(sl.LogRecords) != 2 {
		t.Fatalf("scope: %v (%d records)", sl.Scope, len(sl.LogRecords))
	}
	lr := sl.LogRecords[0]
	a := attrMap(lr.Attributes)
	if lr.Body.GetStringValue() != "hello" || lr.SeverityNumber != 17 || lr.SeverityText != "ERROR" ||
		lr.TimeUnixNano != uint64(ts.UnixNano()) || lr.ObservedTimeUnixNano == 0 ||
		hex.EncodeToString(lr.TraceId) != recs[0].TraceID || hex.EncodeToString(lr.SpanId) != recs[0].SpanID || lr.Flags != 1 ||
		a["n"].GetIntValue() != 42 || !a["ok"].GetBoolValue() || a["f"].GetDoubleValue() != 1.5 ||
		len(a["list"].GetArrayValue().Values) != 2 || a["nested"].GetKvlistValue().Values[0].Key != "x" || a["nil"] != nil ||
		a["weird"] == nil {
		t.Fatalf("record: %v", lr)
	}
	other := reqs[1].ResourceLogs[0].ScopeLogs[0].LogRecords[0]
	if other.SeverityNumber != 0 || len(other.TraceId) != 0 || other.ObservedTimeUnixNano != uint64(ts.Add(time.Second).UnixNano()) {
		t.Fatalf("other: %v", other)
	}
	if got := sink.request(0).Header.Get("Authorization"); got != "Bearer "+testToken {
		t.Fatalf("auth header %q", got)
	}

	if _, err := e.Export(t.Context(), recs[:1], ExportOptions{Gzip: false}); err != nil || sink.gzip[2] {
		t.Fatalf("uncompressed: %v %v", err, sink.gzip)
	}
	if res, err := e.Export(t.Context(), nil, ExportOptions{}); err != nil || res.Accepted != 0 {
		t.Fatal("empty export")
	}
	if err := e.Shutdown(t.Context()); err != nil {
		t.Fatal(err)
	}
	if err := e.Shutdown(t.Context()); err != nil {
		t.Fatal("second shutdown")
	}
	if _, err := e.Export(t.Context(), recs, ExportOptions{}); !errors.Is(err, ErrCancelled) {
		t.Fatalf("after shutdown: %v", err)
	}
}

func TestOTLPLogExporterHTTPErrors(t *testing.T) {
	rec := []LogRecord{{Body: "x", Severity: SeverityInfo}}

	t.Run("partial success", func(t *testing.T) {
		sink := newLogSink(t, func(w http.ResponseWriter, _ int) bool {
			w.Header().Set("Content-Type", "application/x-protobuf")
			b, _ := proto.Marshal(&collogspb.ExportLogsServiceResponse{PartialSuccess: &collogspb.ExportLogsPartialSuccess{RejectedLogRecords: 1, ErrorMessage: "too old"}})
			_, _ = w.Write(b)
			return true
		})
		res, err := testExporter(t, sink.URL, nil).Export(t.Context(), append(rec, rec...), ExportOptions{})
		if err != nil || res.Accepted != 1 || res.Rejected != 1 || res.Error == nil || !strings.Contains(res.Error.Message, "too old") {
			t.Fatalf("partial: %+v %v", res, err)
		}
	})

	t.Run("status mapping", func(t *testing.T) {
		for name, want := range map[string]error{
			"ping.http_unauthenticated_wrong_token": ErrAuthentication,
			"export.http_payload_too_large":         ErrValidation,
			"export.http_bad_request_json":          ErrValidation,
			"export.http_not_found":                 ErrNotFound,
		} {
			f := loadFixture(t, name)
			sink := newLogSink(t, func(w http.ResponseWriter, _ int) bool { f.serve(w); return true })
			_, err := testExporter(t, sink.URL, nil).Export(t.Context(), rec, ExportOptions{})
			if !errors.Is(err, want) {
				t.Fatalf("%s: %v", name, err)
			}
			if name == "ping.http_unauthenticated_wrong_token" && !strings.Contains(err.Error(), "provided authorization does not match") {
				t.Fatalf("message: %v", err)
			}
		}
	})

	t.Run("retryable status after the exporter's own retries", func(t *testing.T) {
		sink := newLogSink(t, func(w http.ResponseWriter, _ int) bool {
			w.Header().Set("Retry-After", "2")
			w.WriteHeader(http.StatusServiceUnavailable)
			return true
		})
		ctx, cancel := context.WithTimeout(t.Context(), 300*time.Millisecond)
		defer cancel()
		_, err := testExporter(t, sink.URL, nil).Export(ctx, rec, ExportOptions{})
		var e *Error
		if !errors.As(err, &e) || e.Code != ErrorCodeUnavailable || !e.Retryable || e.RetryAfter != 2*time.Second {
			t.Fatalf("503: %v", err)
		}
	})

	t.Run("network error", func(t *testing.T) {
		e := testExporter(t, "http://"+closedAddr(t), nil)
		ctx, cancel := context.WithTimeout(t.Context(), 200*time.Millisecond)
		defer cancel()
		_, err := e.Export(ctx, rec, ExportOptions{})
		if !errors.Is(err, ErrTimeout) && !errors.Is(err, ErrUnavailable) {
			t.Fatalf("network: %v", err)
		}
		cctx, ccancel := context.WithCancel(t.Context())
		ccancel()
		if _, err := e.Export(cctx, rec, ExportOptions{}); !errors.Is(err, ErrCancelled) {
			t.Fatalf("cancelled: %v", err)
		}
	})

	t.Run("invalid config", func(t *testing.T) {
		if _, err := NewOTLPLogExporter(Config{Protocol: ProtocolNative}); !errors.Is(err, ErrConfig) {
			t.Fatal(err)
		}
		if _, err := NewOTLPLogExporter(Config{CommonConfig: CommonConfig{TLS: TLSConfig{CACertPEM: "bad"}}}); !errors.Is(err, ErrConfig) {
			t.Fatal(err)
		}
		if _, err := NewLogClient(Config{Protocol: ProtocolNative}, LogClientOptions{}); !errors.Is(err, ErrConfig) {
			t.Fatal(err)
		}
		if _, err := NewLogClient(Config{}, LogClientOptions{Batch: BatchOptions{DropPolicy: "nope"}}); !errors.Is(err, ErrConfig) {
			t.Fatal(err)
		}
	})
}

func TestOTLPLogExporterGRPC(t *testing.T) {
	srv := newOTLPServer(t)
	e := testExporter(t, "http://127.0.0.1:1", func(c *Config) {
		c.Protocol = ProtocolGRPC
		c.GRPCURL = srv.url()
		c.Headers = map[string]string{"X-Scope": "team"}
	})
	rec := []LogRecord{{Body: "grpc hello", Severity: SeverityWarn, Resource: map[string]AttrValue{"service.name": "g"}}}
	res, err := e.Export(t.Context(), rec, ExportOptions{Gzip: true})
	if err != nil || res.Accepted != 1 {
		t.Fatalf("export: %+v %v", res, err)
	}
	_, _, logs, md := srv.snapshot()
	if len(logs) != 1 || logs[0].ResourceLogs[0].ScopeLogs[0].LogRecords[0].Body.GetStringValue() != "grpc hello" || md[0].Get("x-scope")[0] != "team" {
		t.Fatalf("server: %v %v", logs, md)
	}

	srv.mu.Lock()
	srv.rejected = 1
	srv.mu.Unlock()
	res, err = e.Export(t.Context(), append(rec, rec...), ExportOptions{Gzip: false})
	if err != nil || res.Rejected != 1 || res.Accepted != 1 || !strings.Contains(res.Error.Message, "too old") {
		t.Fatalf("partial: %+v %v", res, err)
	}

	srv.mu.Lock()
	srv.rejected = 0
	srv.fail = func(int) error {
		return grpcErr(codes.Unauthenticated, "missing or empty authorization header: Authorization")
	}
	srv.mu.Unlock()
	if _, err := e.Export(t.Context(), rec, ExportOptions{Gzip: true}); !errors.Is(err, ErrAuthentication) {
		t.Fatalf("unauthenticated: %v", err)
	}
	srv.mu.Lock()
	srv.fail = func(int) error {
		return grpcErr(codes.Internal, "grpc: failed to unmarshal the received message: unexpected EOF")
	}
	srv.mu.Unlock()
	if _, err := e.Export(t.Context(), rec, ExportOptions{Gzip: true}); !errors.Is(err, ErrValidation) {
		t.Fatalf("internal: %v", err)
	}
}

func TestCollectorLogClient(t *testing.T) {
	sink := newLogSink(t, nil)
	c := newTestClient(t, sink.URL, func(cfg *Config) {
		cfg.ServiceName = "log-svc"
		cfg.ServiceVersion = "1.0.0"
		cfg.Environment = "test"
		cfg.ResourceAttributes = map[string]any{"team": "core"}
		cfg.LogBatch = BatchOptions{FlushInterval: time.Hour}
	})
	lc, err := c.LogClient(LogClientOptions{})
	if err != nil {
		t.Fatal(err)
	}
	lc.Logger("checkout", map[string]AttrValue{"component": "cart"}).Info(t.Context(), "paid", "password", "hunter2", "amount", 3)
	if err := lc.Flush(t.Context()); err != nil {
		t.Fatal(err)
	}
	reqs := sink.requests()
	if len(reqs) != 1 {
		t.Fatalf("requests: %d", len(reqs))
	}
	rl := reqs[0].ResourceLogs[0]
	ra := attrMap(rl.Resource.Attributes)
	lr := rl.ScopeLogs[0].LogRecords[0]
	la := attrMap(lr.Attributes)
	if ra["service.name"].GetStringValue() != "log-svc" || ra["service.version"].GetStringValue() != "1.0.0" ||
		ra["deployment.environment.name"].GetStringValue() != "test" || ra["team"].GetStringValue() != "core" ||
		lr.Body.GetStringValue() != "paid" || la["password"].GetStringValue() != "[REDACTED]" || la["amount"].GetIntValue() != 3 ||
		la["component"].GetStringValue() != "cart" {
		t.Fatalf("record: %v / %v", rl.Resource, lr)
	}
	if lc.Stats().Sent != 1 {
		t.Fatalf("stats: %+v", lc.Stats())
	}
	exp, err := c.LogExporter()
	if err != nil {
		t.Fatal(err)
	}
	if err := c.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := exp.Export(t.Context(), []LogRecord{{Body: "x"}}, ExportOptions{}); !errors.Is(err, ErrCancelled) {
		t.Fatalf("exporter closed by client: %v", err)
	}
	lc.Send(t.Context(), LogInput{Message: "after close"})
	if lc.Stats().Dropped != 1 {
		t.Fatalf("log client closed by client: %+v", lc.Stats())
	}
}

func TestMergeBatch(t *testing.T) {
	base := BatchOptions{MaxQueueSize: 10, MaxBatchSize: 5, MaxBatchBytes: 100, FlushInterval: time.Second, ExportTimeout: time.Minute, DropPolicy: DropOldest}
	if got := mergeBatch(base, BatchOptions{}); got.MaxQueueSize != 10 || got.MaxBatchSize != 5 || got.MaxBatchBytes != 100 ||
		got.FlushInterval != time.Second || got.ExportTimeout != time.Minute || got.DropPolicy != DropOldest {
		t.Fatalf("merge: %+v", got)
	}
	if got := mergeBatch(base, BatchOptions{MaxQueueSize: 1}); got.MaxQueueSize != 1 {
		t.Fatal("explicit wins")
	}
	if attrValue(uint8(3)).AsString() != "3" {
		t.Fatal("fallback attr value")
	}
	if resourceKey(map[string]AttrValue{"bad": make(chan int)}) == "" {
		t.Fatal("resource key fallback")
	}
}
