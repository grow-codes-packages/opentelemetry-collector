package otelcollector

import (
	"bytes"
	"context"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"sort"
	"sync"
	"time"

	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/exporters/otlp/otlplog/otlploggrpc"
	"go.opentelemetry.io/otel/exporters/otlp/otlplog/otlploghttp"
	otellog "go.opentelemetry.io/otel/log"
	sdklog "go.opentelemetry.io/otel/sdk/log"
	"go.opentelemetry.io/otel/sdk/resource"
	"go.opentelemetry.io/otel/trace"
	collogspb "go.opentelemetry.io/proto/otlp/collector/logs/v1"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"
)

// InstrumentationScope is the scope name of records exported by OTLPLogExporter and of
// the metric helpers' default meter version.
const InstrumentationScope = "grow-codes-opentelemetry-collector"

// maxCapturedBody bounds the response body kept for error mapping.
const maxCapturedBody = 64 << 10

// exportCapture records the last OTLP response of one Export call (HTTP status, headers
// and body, or the gRPC reply and error) so that the official exporters' results can be
// mapped to SDK errors and partial successes.
type exportCapture struct {
	mu       sync.Mutex
	status   int
	header   http.Header
	body     []byte
	grpcSeen bool
	grpcErr  error
	grpcResp *collogspb.ExportLogsServiceResponse
}

type captureKey struct{}

func captureFrom(ctx context.Context) *exportCapture {
	c, _ := ctx.Value(captureKey{}).(*exportCapture)
	return c
}

// captureTransport wraps the HTTP transport of the official exporter.
type captureTransport struct{ base http.RoundTripper }

// RoundTrip implements http.RoundTripper.
func (t captureTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	resp, err := t.base.RoundTrip(req)
	if err != nil {
		return resp, err
	}
	if c := captureFrom(req.Context()); c != nil {
		body, _ := io.ReadAll(io.LimitReader(resp.Body, maxCapturedBody))
		_ = resp.Body.Close()
		resp.Body = io.NopCloser(bytes.NewReader(body))
		c.mu.Lock()
		c.status, c.header, c.body = resp.StatusCode, resp.Header.Clone(), body
		c.mu.Unlock()
	}
	return resp, nil
}

// captureInterceptor records the gRPC reply and error of the official exporter.
func captureInterceptor(ctx context.Context, method string, req, reply any, cc *grpc.ClientConn, invoker grpc.UnaryInvoker, opts ...grpc.CallOption) error {
	err := invoker(ctx, method, req, reply, cc, opts...)
	if c := captureFrom(ctx); c != nil {
		c.mu.Lock()
		c.grpcSeen, c.grpcErr = true, err
		if r, ok := reply.(*collogspb.ExportLogsServiceResponse); ok && err == nil {
			c.grpcResp = r
		}
		c.mu.Unlock()
	}
	return err
}

// collector is an sdklog.Processor that hands emitted records to the slot in the
// context of the Emit call.
type collector struct{}

type collectKey struct{}

func (collector) Enabled(context.Context, sdklog.EnabledParameters) bool { return true }
func (collector) Shutdown(context.Context) error                         { return nil }
func (collector) ForceFlush(context.Context) error                       { return nil }
func (collector) OnEmit(ctx context.Context, r *sdklog.Record) error {
	if slot, ok := ctx.Value(collectKey{}).(*[]sdklog.Record); ok {
		*slot = append(*slot, r.Clone())
	}
	return nil
}

// OTLPLogExporter implements LogExporter with the official OpenTelemetry OTLP log
// exporters (otlploghttp: protobuf over HTTP to the logs endpoint, or otlploggrpc:
// LogsService/Export), per Config.Protocol. Records keep their own resource; the
// exporters' OTLP retry (429/502/503/504, gRPC UNAVAILABLE…) applies. A partial
// success (rejected_log_records) is reported in ExportResult and never resent. It is
// safe for concurrent use.
type OTLPLogExporter struct {
	cfg       Config
	op        string
	transport *httpTransport
	secrets   []string

	mu        sync.Mutex
	shutdown  bool
	exporters map[bool]sdklog.Exporter // keyed by gzip
	providers map[string]*sdklog.LoggerProvider
	now       func() time.Time
}

var _ LogExporter = (*OTLPLogExporter)(nil)

// NewOTLPLogExporter returns an exporter for cfg (LogsURL or URL + /v1/logs, or
// GRPCURL). It makes no network call; Shutdown releases its connections.
func NewOTLPLogExporter(cfg Config) (*OTLPLogExporter, error) {
	if err := cfg.Validate(); err != nil {
		return nil, err
	}
	cfg = cfg.WithDefaults()
	t, err := newHTTPTransport(cfg.CommonConfig)
	if err != nil {
		return nil, err
	}
	secrets := cfg.secretValues()
	t.addSecret(secrets...)
	return &OTLPLogExporter{
		cfg: cfg, op: "logs.export", transport: t, secrets: secrets,
		exporters: map[bool]sdklog.Exporter{}, providers: map[string]*sdklog.LoggerProvider{}, now: time.Now,
	}, nil
}

// exporter returns the official exporter for the compression choice (created lazily).
func (e *OTLPLogExporter) exporter(ctx context.Context, gzip bool) (sdklog.Exporter, error) {
	if exp, ok := e.exporters[gzip]; ok {
		return exp, nil
	}
	cfg := e.cfg
	if !gzip {
		cfg.Compression = CompressionNone
	}
	exp, err := newOTelLogExporterWith(ctx, cfg, e.transport)
	if err != nil {
		return nil, err
	}
	e.exporters[gzip] = exp
	return exp, nil
}

// provider returns the LoggerProvider of a resource (created lazily).
func (e *OTLPLogExporter) provider(attrs map[string]AttrValue) (*sdklog.LoggerProvider, string) {
	key := resourceKey(attrs)
	if p, ok := e.providers[key]; ok {
		return p, key
	}
	res := resource.NewSchemaless(attributesFrom(attrs)...)
	p := sdklog.NewLoggerProvider(sdklog.WithResource(res), sdklog.WithProcessor(collector{}),
		sdklog.WithAttributeCountLimit(-1), sdklog.WithAttributeValueLengthLimit(-1))
	e.providers[key] = p
	return p, key
}

// Export sends records in one OTLP request per distinct resource. Accepted =
// len(records) - rejected.
func (e *OTLPLogExporter) Export(ctx context.Context, records []LogRecord, opts ExportOptions) (ExportResult, error) {
	e.mu.Lock()
	defer e.mu.Unlock()
	if e.shutdown {
		return ExportResult{}, &Error{Code: ErrorCodeCancelled, Message: e.op + ": exporter is shut down"}
	}
	if len(records) == 0 {
		return ExportResult{}, nil
	}
	exp, err := e.exporter(ctx, opts.Gzip)
	if err != nil {
		return ExportResult{}, err
	}
	// Group by resource (first-seen order) and convert through the SDK.
	var order []string
	groups := map[string][]sdklog.Record{}
	now := e.now()
	for _, r := range records {
		p, key := e.provider(r.Resource)
		if _, seen := groups[key]; !seen {
			order = append(order, key)
		}
		slot := groups[key]
		emitCtx := context.WithValue(contextWithRecordSpan(context.Background(), r), collectKey{}, &slot)
		rec := apiRecord(r, now)
		p.Logger(InstrumentationScope, otellog.WithInstrumentationVersion(Version)).Emit(emitCtx, rec)
		groups[key] = slot
	}
	var rejected int64
	var messages []string
	for _, key := range order {
		capture := &exportCapture{}
		cctx := context.WithValue(ctx, captureKey{}, capture)
		eerr := exp.Export(cctx, groups[key])
		p, err := e.mapResult(ctx, capture, eerr)
		if err != nil {
			return ExportResult{}, err
		}
		rejected += p.rejected
		if p.message != "" {
			messages = append(messages, p.message)
		}
	}
	n := int(min(max(rejected, 0), int64(len(records))))
	res := ExportResult{Accepted: len(records) - n, Rejected: n}
	if n > 0 || len(messages) > 0 {
		msg := fmt.Sprintf("%s: %d of %d records rejected", e.op, n, len(records))
		if len(messages) > 0 {
			msg += ": " + truncate(RedactText(messages[0], e.secrets), maxVendorText)
		}
		res.Error = &Error{Code: ErrorCodeValidation, Message: msg}
	}
	return res, nil
}

// mapResult turns the captured response (or the exporter error) into a partial
// success or an *Error.
func (e *OTLPLogExporter) mapResult(ctx context.Context, c *exportCapture, err error) (partial, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.grpcSeen {
		if c.grpcErr != nil {
			return partial{}, grpcError(ctx, e.op, c.grpcErr, e.secrets)
		}
		ps := c.grpcResp.GetPartialSuccess()
		return partial{rejected: ps.GetRejectedLogRecords(), message: ps.GetErrorMessage()}, nil
	}
	if c.status != 0 {
		if c.status >= 200 && c.status < 300 {
			return decodePartialSuccess(c.body), nil
		}
		res := &response{Status: c.status, Header: c.header, Body: c.body, RequestID: requestID(c.header)}
		se := StatusError(e.op, c.status, res.RequestID, otlpErrorText(res), e.secrets)
		if c.status == http.StatusTooManyRequests || c.status == http.StatusServiceUnavailable {
			if d, ok := ParseRetryAfter(c.header.Get("Retry-After"), time.Now()); ok {
				se.RetryAfter = d
			}
		}
		return partial{}, se
	}
	if err == nil {
		return partial{}, nil
	}
	if ctx.Err() != nil || errors.Is(err, context.DeadlineExceeded) || errors.Is(err, context.Canceled) {
		cause := ctx.Err()
		if cause == nil {
			cause = context.DeadlineExceeded
		}
		return partial{}, contextErr(e.op, cause)
	}
	return partial{}, &Error{Code: ErrorCodeUnavailable, Message: e.op + ": " + RedactText(err.Error(), e.secrets), Retryable: true}
}

// Shutdown shuts the official exporters down; later exports fail with CANCELLED.
// Idempotent.
func (e *OTLPLogExporter) Shutdown(ctx context.Context) error {
	e.mu.Lock()
	defer e.mu.Unlock()
	if e.shutdown {
		return nil
	}
	e.shutdown = true
	var errs []error
	for _, exp := range e.exporters {
		errs = append(errs, exp.Shutdown(ctx))
	}
	for _, p := range e.providers {
		_ = p.Shutdown(ctx)
	}
	e.transport.close()
	if err := errors.Join(errs...); err != nil {
		return ContextError(e.op, err)
	}
	return nil
}

// newOTelLogExporterWith builds the official OTLP log exporter for cfg. For HTTP it
// uses t's connection settings (TLS, proxy, connect timeout) with a capturing
// transport; for gRPC a capturing interceptor.
func newOTelLogExporterWith(ctx context.Context, cfg Config, t *httpTransport) (sdklog.Exporter, error) {
	endpoint := cfg.SignalEndpoint(SignalLogs)
	headers := cfg.SignalHeaders(SignalLogs)
	if cfg.Protocol == ProtocolGRPC {
		opts := []otlploggrpc.Option{
			otlploggrpc.WithEndpointURL(endpoint), otlploggrpc.WithTimeout(cfg.ExportTimeout),
			otlploggrpc.WithDialOption(grpc.WithUnaryInterceptor(captureInterceptor), grpc.WithUserAgent(UserAgent)),
		}
		if len(headers) > 0 {
			opts = append(opts, otlploggrpc.WithHeaders(headers))
		}
		if cfg.Compression == CompressionGzip {
			opts = append(opts, otlploggrpc.WithCompressor("gzip"))
		}
		if isSecure(endpoint) {
			opts = append(opts, otlploggrpc.WithTLSCredentials(credentials.NewTLS(t.transport.TLSClientConfig)))
		} else {
			opts = append(opts, otlploggrpc.WithInsecure())
		}
		return otlploggrpc.New(ctx, opts...)
	}
	opts := []otlploghttp.Option{
		otlploghttp.WithEndpointURL(endpoint),
		otlploghttp.WithHTTPClient(&http.Client{Transport: captureTransport{base: t.transport}, Timeout: cfg.ExportTimeout}),
	}
	if len(headers) > 0 {
		opts = append(opts, otlploghttp.WithHeaders(headers))
	}
	if cfg.Compression == CompressionGzip {
		opts = append(opts, otlploghttp.WithCompression(otlploghttp.GzipCompression))
	} else {
		opts = append(opts, otlploghttp.WithCompression(otlploghttp.NoCompression))
	}
	return otlploghttp.New(ctx, opts...)
}

// contextWithRecordSpan puts the record's trace context into ctx (the SDK reads the
// trace and span id of a log record from the emit context).
func contextWithRecordSpan(ctx context.Context, r LogRecord) context.Context {
	var tid trace.TraceID
	var sid trace.SpanID
	if b, err := hex.DecodeString(r.TraceID); err == nil && len(b) == len(tid) {
		copy(tid[:], b)
	}
	if b, err := hex.DecodeString(r.SpanID); err == nil && len(b) == len(sid) {
		copy(sid[:], b)
	}
	if !tid.IsValid() && !sid.IsValid() {
		return ctx
	}
	var flags trace.TraceFlags
	if r.TraceFlags != nil {
		flags = trace.TraceFlags(*r.TraceFlags)
	}
	sc := trace.NewSpanContext(trace.SpanContextConfig{TraceID: tid, SpanID: sid, TraceFlags: flags})
	return trace.ContextWithSpanContext(ctx, sc)
}

// apiRecord converts a LogRecord to an OpenTelemetry log API record.
func apiRecord(r LogRecord, now time.Time) otellog.Record {
	var rec otellog.Record
	rec.SetTimestamp(r.Timestamp)
	observed := r.ObservedTimestamp
	if observed.IsZero() {
		observed = now
	}
	rec.SetObservedTimestamp(observed)
	sev := r.Severity
	if sev < 0 || sev > 24 {
		sev = SeverityUnspecified
	}
	rec.SetSeverity(otellog.Severity(sev))
	rec.SetSeverityText(r.SeverityText)
	rec.SetBody(attribute.StringValue(r.Body))
	rec.AddAttributes(attributesFrom(r.Attributes)...)
	return rec
}

// LogExporter returns a new OTLP log exporter for the client's configuration. Client
// Close shuts it down.
func (c *Client) LogExporter() (*OTLPLogExporter, error) {
	e, err := NewOTLPLogExporter(c.cfg)
	if err != nil {
		return nil, err
	}
	if err := c.register(e.Shutdown); err != nil {
		_ = e.Shutdown(context.Background())
		return nil, err
	}
	return e, nil
}

// LogClient returns a BatchingLogClient that exports OTLP logs to the collector (see
// the package function NewLogClient). Client Close flushes and closes it.
func (c *Client) LogClient(opts LogClientOptions) (*BatchingLogClient, error) {
	lc, err := NewLogClient(c.cfg, opts)
	if err != nil {
		return nil, err
	}
	if err := c.register(lc.Close); err != nil {
		_ = lc.Close(context.Background())
		return nil, err
	}
	return lc, nil
}

// NewLogClient returns a BatchingLogClient on an OTLPLogExporter for cfg. Unless
// opts.Resource is set, every record gets the configured resource (service.name,
// service.version, deployment.environment.name and ResourceAttributes); unset batch
// fields come from cfg.LogBatch (OTEL_BLRP_*). Close flushes it and releases the
// exporter.
func NewLogClient(cfg Config, opts LogClientOptions) (*BatchingLogClient, error) {
	exp, err := NewOTLPLogExporter(cfg)
	if err != nil {
		return nil, err
	}
	d := exp.cfg
	if opts.Resource == nil {
		opts.Resource = d.resourceMap()
	}
	opts.Batch = mergeBatch(d.LogBatch, opts.Batch)
	lc, err := NewBatchingLogClient(exp, opts)
	if err != nil {
		_ = exp.Shutdown(context.Background())
		return nil, err
	}
	return lc, nil
}

// mergeBatch fills the unset fields of explicit from base.
func mergeBatch(base, explicit BatchOptions) BatchOptions {
	out := explicit
	if out.MaxQueueSize == 0 {
		out.MaxQueueSize = base.MaxQueueSize
	}
	if out.MaxBatchSize == 0 {
		out.MaxBatchSize = base.MaxBatchSize
	}
	if out.MaxBatchBytes == 0 {
		out.MaxBatchBytes = base.MaxBatchBytes
	}
	if out.FlushInterval == 0 {
		out.FlushInterval = base.FlushInterval
	}
	if out.ExportTimeout == 0 {
		out.ExportTimeout = base.ExportTimeout
	}
	if out.DropPolicy == "" {
		out.DropPolicy = base.DropPolicy
	}
	return out
}

// resourceMap returns the configured resource attributes as a map.
func (c Config) resourceMap() map[string]AttrValue {
	out := make(map[string]AttrValue, len(c.ResourceAttributes)+3)
	for k, v := range c.ResourceAttributes {
		out[k] = v
	}
	out["service.name"] = c.ServiceName
	if c.ServiceVersion != "" {
		out["service.version"] = c.ServiceVersion
	}
	if c.Environment != "" {
		out["deployment.environment.name"] = c.Environment
	}
	return out
}

func resourceKey(m map[string]AttrValue) string {
	b, err := json.Marshal(NormalizeAttributes(m))
	if err != nil {
		return fmt.Sprint(m)
	}
	return string(b)
}

// attributesFrom converts an attribute map to OpenTelemetry attributes sorted by key
// (maps and mixed slices keep their structure).
func attributesFrom(m map[string]AttrValue) []attribute.KeyValue {
	keys := make([]string, 0, len(m))
	for k, v := range m {
		if v != nil {
			keys = append(keys, k)
		}
	}
	sort.Strings(keys)
	out := make([]attribute.KeyValue, 0, len(keys))
	for _, k := range keys {
		out = append(out, attribute.KeyValue{Key: attribute.Key(k), Value: attrValue(NormalizeAttrValue(m[k]))})
	}
	return out
}

// attrValue converts a normalized AttrValue to an attribute.Value.
func attrValue(v AttrValue) attribute.Value {
	switch x := v.(type) {
	case string:
		return attribute.StringValue(x)
	case bool:
		return attribute.BoolValue(x)
	case int64:
		return attribute.Int64Value(x)
	case float64:
		return attribute.Float64Value(x)
	case []AttrValue:
		vals := make([]attribute.Value, 0, len(x))
		for _, it := range x {
			vals = append(vals, attrValue(it))
		}
		return attribute.SliceValue(vals...)
	case map[string]AttrValue:
		return attribute.MapValue(attributesFrom(x)...)
	}
	return attribute.StringValue(fmt.Sprint(v))
}
