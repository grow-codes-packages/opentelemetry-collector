package otelcollector

import (
	"bytes"
	"compress/gzip"
	"context"
	"encoding/base64"
	"encoding/json"
	"io"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"sync"
	"testing"

	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/metadata"
	grpcstatus "google.golang.org/grpc/status"
	"google.golang.org/protobuf/proto"

	collogspb "go.opentelemetry.io/proto/otlp/collector/logs/v1"
	colmetricspb "go.opentelemetry.io/proto/otlp/collector/metrics/v1"
	coltracepb "go.opentelemetry.io/proto/otlp/collector/trace/v1"
)

// fixture is a response recorded from the real collector 0.165.0
// (testdata/fixtures/<function>.<case>.json).
type fixture struct {
	Response struct {
		Status      int               `json:"status"`
		Headers     map[string]string `json:"headers"`
		GRPCStatus  int               `json:"grpcStatus"`
		GRPCMessage string            `json:"grpcMessage"`
		Body        struct {
			JSON   json.RawMessage `json:"json"`
			Text   string          `json:"text"`
			Base64 string          `json:"base64"`
		} `json:"body"`
		BodyExcerpt []string `json:"bodyExcerpt"`
	} `json:"response"`
}

func loadFixture(t *testing.T, name string) fixture {
	t.Helper()
	data, err := os.ReadFile(filepath.Join("testdata", "fixtures", name+".json"))
	if err != nil {
		t.Fatalf("fixture %s: %v", name, err)
	}
	var f fixture
	if err := json.Unmarshal(data, &f); err != nil {
		t.Fatalf("fixture %s: %v", name, err)
	}
	return f
}

// body returns the recorded body bytes.
func (f fixture) body() []byte {
	b := f.Response.Body
	switch {
	case len(b.JSON) > 0:
		return b.JSON
	case b.Base64 != "":
		raw, _ := base64.StdEncoding.DecodeString(b.Base64)
		return raw
	case b.Text != "":
		return []byte(b.Text)
	}
	var buf bytes.Buffer
	for _, line := range f.Response.BodyExcerpt {
		buf.WriteString(line + "\n")
	}
	return buf.Bytes()
}

// serve writes the recorded response (Content-Length is recomputed).
func (f fixture) serve(w http.ResponseWriter) {
	for k, v := range f.Response.Headers {
		if k == "Content-Length" || k == "Transfer-Encoding" || k == "Connection" {
			continue
		}
		w.Header().Set(k, v)
	}
	w.WriteHeader(f.Response.Status)
	_, _ = w.Write(f.body())
}

func serveFixture(t *testing.T, name string) func(w http.ResponseWriter, r *http.Request, n int) {
	f := loadFixture(t, name)
	return func(w http.ResponseWriter, _ *http.Request, _ int) { f.serve(w) }
}

// readBody returns the (gunzipped) request body.
func readBody(t *testing.T, r *http.Request) []byte {
	t.Helper()
	var rd io.Reader = r.Body
	if r.Header.Get("Content-Encoding") == "gzip" {
		zr, err := gzip.NewReader(r.Body)
		if err != nil {
			t.Errorf("gzip: %v", err)
			return nil
		}
		rd = zr
	}
	b, err := io.ReadAll(rd)
	if err != nil {
		t.Errorf("read body: %v", err)
	}
	return b
}

// otlpServer is an in-process OTLP/gRPC server for traces, metrics and logs.
type otlpServer struct {
	addr string
	srv  *grpc.Server

	mu       sync.Mutex
	traces   []*coltracepb.ExportTraceServiceRequest
	metrics  []*colmetricspb.ExportMetricsServiceRequest
	logs     []*collogspb.ExportLogsServiceRequest
	metadata []metadata.MD
	// fail returns an error for the n-th call (1-based) of any service; nil = none.
	fail func(n int) error
	// partial is returned as rejected count on success.
	rejected int64
	calls    int
}

func newOTLPServer(t *testing.T) *otlpServer {
	t.Helper()
	lis, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	s := &otlpServer{addr: lis.Addr().String(), srv: grpc.NewServer()}
	coltracepb.RegisterTraceServiceServer(s.srv, traceSvc{otlpServer: s})
	colmetricspb.RegisterMetricsServiceServer(s.srv, metricsSvc{otlpServer: s})
	collogspb.RegisterLogsServiceServer(s.srv, logsSvc{otlpServer: s})
	go func() { _ = s.srv.Serve(lis) }()
	t.Cleanup(s.srv.Stop)
	return s
}

func (s *otlpServer) url() string { return "http://" + s.addr }

func (s *otlpServer) record(ctx context.Context, req proto.Message) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.calls++
	md, _ := metadata.FromIncomingContext(ctx)
	s.metadata = append(s.metadata, md)
	if s.fail != nil {
		if err := s.fail(s.calls); err != nil {
			return err
		}
	}
	switch r := req.(type) {
	case *coltracepb.ExportTraceServiceRequest:
		s.traces = append(s.traces, r)
	case *colmetricspb.ExportMetricsServiceRequest:
		s.metrics = append(s.metrics, r)
	case *collogspb.ExportLogsServiceRequest:
		s.logs = append(s.logs, r)
	}
	return nil
}

type traceSvc struct {
	coltracepb.UnimplementedTraceServiceServer
	*otlpServer
}

type metricsSvc struct {
	colmetricspb.UnimplementedMetricsServiceServer
	*otlpServer
}

type logsSvc struct {
	collogspb.UnimplementedLogsServiceServer
	*otlpServer
}

func (s traceSvc) Export(ctx context.Context, req *coltracepb.ExportTraceServiceRequest) (*coltracepb.ExportTraceServiceResponse, error) {
	if err := s.record(ctx, req); err != nil {
		return nil, err
	}
	resp := &coltracepb.ExportTraceServiceResponse{}
	if s.rejected > 0 {
		resp.PartialSuccess = &coltracepb.ExportTracePartialSuccess{RejectedSpans: s.rejected, ErrorMessage: "x"}
	}
	return resp, nil
}

func (s metricsSvc) Export(ctx context.Context, req *colmetricspb.ExportMetricsServiceRequest) (*colmetricspb.ExportMetricsServiceResponse, error) {
	if err := s.record(ctx, req); err != nil {
		return nil, err
	}
	return &colmetricspb.ExportMetricsServiceResponse{}, nil
}

func (s logsSvc) Export(ctx context.Context, req *collogspb.ExportLogsServiceRequest) (*collogspb.ExportLogsServiceResponse, error) {
	if err := s.record(ctx, req); err != nil {
		return nil, err
	}
	resp := &collogspb.ExportLogsServiceResponse{}
	if s.rejected > 0 {
		resp.PartialSuccess = &collogspb.ExportLogsPartialSuccess{RejectedLogRecords: s.rejected, ErrorMessage: "too old"}
	}
	return resp, nil
}

func (s *otlpServer) snapshot() (traces int, metrics int, logs []*collogspb.ExportLogsServiceRequest, md []metadata.MD) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return len(s.traces), len(s.metrics), append([]*collogspb.ExportLogsServiceRequest(nil), s.logs...), append([]metadata.MD(nil), s.metadata...)
}

func grpcErr(c codes.Code, msg string) error { return grpcstatus.Error(c, msg) }
