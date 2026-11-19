package integration

// Test-only read-back adapters (DECISIONS.md §7): the collector stores nothing, so the
// conformance query clients read traces from the Jaeger sidecar
// (OTELCOL_TEST_JAEGER_URL, /api/v3) and logs from the file exporter output served by
// the capture sidecar (OTELCOL_TEST_CAPTURE_URL/logs.jsonl). They are not shipped.

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"sort"
	"strconv"
	"strings"
	"time"

	otelcollector "github.com/grow-codes-packages/opentelemetry-collector/go"
)

// otlpAnyValue is an OTLP JSON AnyValue.
type otlpAnyValue struct {
	StringValue *string         `json:"stringValue"`
	BoolValue   *bool           `json:"boolValue"`
	IntValue    json.RawMessage `json:"intValue"`
	DoubleValue *float64        `json:"doubleValue"`
	ArrayValue  *struct {
		Values []otlpAnyValue `json:"values"`
	} `json:"arrayValue"`
	KvlistValue *struct {
		Values []otlpKeyValue `json:"values"`
	} `json:"kvlistValue"`
}

type otlpKeyValue struct {
	Key   string       `json:"key"`
	Value otlpAnyValue `json:"value"`
}

func (v otlpAnyValue) value() any {
	switch {
	case v.StringValue != nil:
		return *v.StringValue
	case v.BoolValue != nil:
		return *v.BoolValue
	case len(v.IntValue) > 0:
		n, _ := strconv.ParseInt(strings.Trim(string(v.IntValue), `"`), 10, 64)
		return n
	case v.DoubleValue != nil:
		return *v.DoubleValue
	case v.ArrayValue != nil:
		out := make([]any, 0, len(v.ArrayValue.Values))
		for _, it := range v.ArrayValue.Values {
			out = append(out, it.value())
		}
		return out
	case v.KvlistValue != nil:
		return kvMap(v.KvlistValue.Values)
	}
	return nil
}

func kvMap(kvs []otlpKeyValue) map[string]any {
	out := make(map[string]any, len(kvs))
	for _, kv := range kvs {
		out[kv.Key] = kv.Value.value()
	}
	return out
}

func nanos(raw json.RawMessage) time.Time {
	n, err := strconv.ParseInt(strings.Trim(string(raw), `"`), 10, 64)
	if err != nil || n == 0 {
		return time.Time{}
	}
	return time.Unix(0, n).UTC()
}

func httpGet(ctx context.Context, url string) (int, []byte, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return 0, nil, err
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, nil, otelcollector.NewError(otelcollector.ErrorCodeUnavailable, "readback: "+err.Error(), err)
	}
	defer func() { _ = resp.Body.Close() }()
	body, err := io.ReadAll(resp.Body)
	return resp.StatusCode, body, err
}

// captureLines fetches one capture file and returns its JSON lines.
func captureLines(ctx context.Context, signal string) ([][]byte, error) {
	status, body, err := httpGet(ctx, os.Getenv("OTELCOL_TEST_CAPTURE_URL")+"/"+signal+".jsonl")
	if err != nil {
		return nil, err
	}
	if status == http.StatusNotFound {
		return nil, nil
	}
	if status != http.StatusOK {
		return nil, fmt.Errorf("capture %s: HTTP %d", signal, status)
	}
	var lines [][]byte
	sc := bufio.NewScanner(bytes.NewReader(body))
	sc.Buffer(make([]byte, 0, 1<<20), 64<<20)
	for sc.Scan() {
		if line := bytes.TrimSpace(sc.Bytes()); len(line) > 0 {
			lines = append(lines, append([]byte(nil), line...))
		}
	}
	return lines, sc.Err()
}

// captureLogs parses every log record of logs.jsonl.
func captureLogs(ctx context.Context) ([]otelcollector.LogRecord, error) {
	lines, err := captureLines(ctx, "logs")
	if err != nil {
		return nil, err
	}
	var out []otelcollector.LogRecord
	for _, line := range lines {
		var req struct {
			ResourceLogs []struct {
				Resource struct {
					Attributes []otlpKeyValue `json:"attributes"`
				} `json:"resource"`
				ScopeLogs []struct {
					LogRecords []struct {
						TimeUnixNano         json.RawMessage `json:"timeUnixNano"`
						ObservedTimeUnixNano json.RawMessage `json:"observedTimeUnixNano"`
						SeverityNumber       int             `json:"severityNumber"`
						SeverityText         string          `json:"severityText"`
						Body                 otlpAnyValue    `json:"body"`
						Attributes           []otlpKeyValue  `json:"attributes"`
						TraceID              string          `json:"traceId"`
						SpanID               string          `json:"spanId"`
					} `json:"logRecords"`
				} `json:"scopeLogs"`
			} `json:"resourceLogs"`
		}
		if err := json.Unmarshal(line, &req); err != nil {
			return nil, fmt.Errorf("capture logs: %w", err)
		}
		for _, rl := range req.ResourceLogs {
			res := kvMap(rl.Resource.Attributes)
			for _, sl := range rl.ScopeLogs {
				for _, lr := range sl.LogRecords {
					body, _ := lr.Body.value().(string)
					out = append(out, otelcollector.LogRecord{
						Timestamp: nanos(lr.TimeUnixNano), ObservedTimestamp: nanos(lr.ObservedTimeUnixNano),
						Severity: otelcollector.Severity(lr.SeverityNumber), SeverityText: lr.SeverityText,
						Body: body, Attributes: kvMap(lr.Attributes), Resource: res,
						TraceID: lr.TraceID, SpanID: lr.SpanID,
					})
				}
			}
		}
	}
	return out, nil
}

// captureLogQuery is the conformance LogQueryClient: substring match on the body.
type captureLogQuery struct {
	otelcollector.UnsupportedLogQueryClient
}

func (captureLogQuery) Search(ctx context.Context, query string, opts otelcollector.SearchOptions) (otelcollector.LogSearchResult, error) {
	opts = opts.WithDefaults()
	recs, err := captureLogs(ctx)
	if err != nil {
		return otelcollector.LogSearchResult{}, err
	}
	var matched []otelcollector.LogRecord
	for _, r := range recs {
		if strings.Contains(r.Body, query) {
			matched = append(matched, r)
		}
	}
	if len(matched) > opts.Limit {
		matched = matched[:opts.Limit]
	}
	return otelcollector.LogSearchResult{Records: matched}, nil
}

func (q captureLogQuery) WaitForLog(ctx context.Context, query string, opts otelcollector.WaitForLogOptions) (otelcollector.LogRecord, error) {
	return otelcollector.WaitForLog(ctx, q, query, opts)
}

// jaegerTraceQuery is the conformance TraceQueryClient on the Jaeger v3 API.
type jaegerTraceQuery struct {
	otelcollector.UnsupportedTraceQueryClient
}

func (jaegerTraceQuery) GetTrace(ctx context.Context, traceID string) (*otelcollector.Trace, error) {
	status, body, err := httpGet(ctx, os.Getenv("OTELCOL_TEST_JAEGER_URL")+"/api/v3/traces/"+traceID)
	if err != nil {
		return nil, err
	}
	if status == http.StatusNotFound {
		return nil, nil
	}
	if status != http.StatusOK {
		return nil, fmt.Errorf("jaeger getTrace: HTTP %d", status)
	}
	var doc struct {
		Result struct {
			ResourceSpans []struct {
				Resource struct {
					Attributes []otlpKeyValue `json:"attributes"`
				} `json:"resource"`
				ScopeSpans []struct {
					Spans []struct {
						TraceID           string          `json:"traceId"`
						SpanID            string          `json:"spanId"`
						ParentSpanID      string          `json:"parentSpanId"`
						Name              string          `json:"name"`
						Kind              int             `json:"kind"`
						StartTimeUnixNano json.RawMessage `json:"startTimeUnixNano"`
						EndTimeUnixNano   json.RawMessage `json:"endTimeUnixNano"`
						Attributes        []otlpKeyValue  `json:"attributes"`
						Status            struct {
							Code    int    `json:"code"`
							Message string `json:"message"`
						} `json:"status"`
						Events []struct {
							Name string `json:"name"`
						} `json:"events"`
					} `json:"spans"`
				} `json:"scopeSpans"`
			} `json:"resourceSpans"`
		} `json:"result"`
	}
	if err := json.Unmarshal(body, &doc); err != nil {
		return nil, err
	}
	kinds := []otelcollector.SpanKind{otelcollector.SpanKindUnspecified, otelcollector.SpanKindInternal, otelcollector.SpanKindServer,
		otelcollector.SpanKindClient, otelcollector.SpanKindProducer, otelcollector.SpanKindConsumer}
	statuses := []otelcollector.SpanStatusCode{otelcollector.SpanStatusUnset, otelcollector.SpanStatusOK, otelcollector.SpanStatusError}
	tr := &otelcollector.Trace{TraceID: traceID}
	for _, rs := range doc.Result.ResourceSpans {
		svc, _ := kvMap(rs.Resource.Attributes)["service.name"].(string)
		for _, ss := range rs.ScopeSpans {
			for _, s := range ss.Spans {
				start, end := nanos(s.StartTimeUnixNano), nanos(s.EndTimeUnixNano)
				span := otelcollector.Span{
					TraceID: s.TraceID, SpanID: s.SpanID, ParentSpanID: s.ParentSpanID, Name: s.Name, ServiceName: svc,
					StartMicros: start.UnixMicro(), EndMicros: end.UnixMicro(), DurationMicros: end.Sub(start).Microseconds(),
					Attributes: kvMap(s.Attributes),
				}
				if s.Kind >= 0 && s.Kind < len(kinds) {
					span.Kind = kinds[s.Kind]
				}
				if s.Status.Code >= 0 && s.Status.Code < len(statuses) {
					span.Status = otelcollector.SpanStatus{Code: statuses[s.Status.Code], Message: s.Status.Message}
				}
				for _, ev := range s.Events {
					span.Events = append(span.Events, otelcollector.SpanEvent{Name: ev.Name})
				}
				tr.Spans = append(tr.Spans, span)
			}
		}
	}
	tr.Spans = otelcollector.SortSpans(tr.Spans)
	return tr, nil
}

func (jaegerTraceQuery) ListServices(ctx context.Context) ([]string, error) {
	status, body, err := httpGet(ctx, os.Getenv("OTELCOL_TEST_JAEGER_URL")+"/api/v3/services")
	if err != nil {
		return nil, err
	}
	if status != http.StatusOK {
		return nil, fmt.Errorf("jaeger services: HTTP %d", status)
	}
	var doc struct {
		Services []string `json:"services"`
	}
	if err := json.Unmarshal(body, &doc); err != nil {
		return nil, err
	}
	sort.Strings(doc.Services)
	return doc.Services, nil
}

func (q jaegerTraceQuery) WaitForTrace(ctx context.Context, traceID string, opts otelcollector.WaitForTraceOptions) (*otelcollector.Trace, error) {
	return otelcollector.WaitForTrace(ctx, q, traceID, opts)
}
