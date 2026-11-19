package otelcollector

import (
	"context"
	"fmt"
	"math"
	"net"
	"net/http"
	"net/url"
	"strings"
	"time"

	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials"
	"google.golang.org/grpc/credentials/insecure"
	grpcgzip "google.golang.org/grpc/encoding/gzip"
	"google.golang.org/grpc/metadata"
	grpcstatus "google.golang.org/grpc/status"
	"google.golang.org/protobuf/encoding/protowire"
)

// partial is a decoded OTLP partial_success (rejected items and the error message).
type partial struct {
	rejected int64
	message  string
}

// protoFields decodes the top-level varint and length-delimited fields of a protobuf
// message (first occurrence per field number).
func protoFields(b []byte) (varints map[protowire.Number]uint64, bytesFields map[protowire.Number][]byte, ok bool) {
	varints = map[protowire.Number]uint64{}
	bytesFields = map[protowire.Number][]byte{}
	for len(b) > 0 {
		num, typ, n := protowire.ConsumeTag(b)
		if n < 0 {
			return nil, nil, false
		}
		b = b[n:]
		switch typ {
		case protowire.VarintType:
			v, m := protowire.ConsumeVarint(b)
			if m < 0 {
				return nil, nil, false
			}
			if _, seen := varints[num]; !seen {
				varints[num] = v
			}
			b = b[m:]
		case protowire.BytesType:
			v, m := protowire.ConsumeBytes(b)
			if m < 0 {
				return nil, nil, false
			}
			if _, seen := bytesFields[num]; !seen {
				bytesFields[num] = v
			}
			b = b[m:]
		default:
			m := protowire.ConsumeFieldValue(num, typ, b)
			if m < 0 {
				return nil, nil, false
			}
			b = b[m:]
		}
	}
	return varints, bytesFields, true
}

// decodePartialSuccess reads an Export*ServiceResponse: field 1 partial_success
// {1: rejected count, 2: error_message}.
func decodePartialSuccess(body []byte) partial {
	_, fields, ok := protoFields(body)
	if !ok {
		return partial{}
	}
	ps, present := fields[1]
	if !present {
		return partial{}
	}
	v, b, ok := protoFields(ps)
	if !ok {
		return partial{}
	}
	return partial{rejected: int64(min(v[1], math.MaxInt64)), message: string(b[2])} //nolint:gosec // clamped
}

// decodeRPCStatus reads a google.rpc.Status {1: code, 2: message}.
func decodeRPCStatus(body []byte) (code int, message string, ok bool) {
	v, b, ok := protoFields(body)
	if !ok || (len(v) == 0 && len(b) == 0) {
		return 0, "", false
	}
	return int(min(v[1], math.MaxInt32)), string(b[2]), true //nolint:gosec // clamped
}

// otlpErrorText extracts the error message of an OTLP/HTTP error response: a
// protobuf google.rpc.Status, a JSON {"code","message"} or a text line.
func otlpErrorText(res *response) string {
	if strings.HasPrefix(res.Header.Get("Content-Type"), "application/x-protobuf") {
		if _, msg, ok := decodeRPCStatus(res.Body); ok {
			return msg
		}
		return ""
	}
	return vendorErrorText(res)
}

// postOTLP sends one OTLP/HTTP protobuf export to ep and maps the response.
func postOTLP(ctx context.Context, ep *endpoint, op string, body []byte) (partial, error) {
	res, err := ep.t.do(ctx, &request{
		Method: http.MethodPost, Path: ep.path, Body: body, ContentType: "application/x-protobuf",
		Header: http.Header{"Accept": {"application/x-protobuf"}}, Operation: op, RawStatus: true,
	})
	if err != nil {
		return partial{}, err
	}
	if res.Status >= 200 && res.Status < 300 {
		return decodePartialSuccess(res.Body), nil
	}
	e := StatusError(op, res.Status, res.RequestID, otlpErrorText(res), ep.t.knownSecrets())
	if res.Status == http.StatusTooManyRequests || res.Status == http.StatusServiceUnavailable {
		if d, ok := ParseRetryAfter(res.Header.Get("Retry-After"), time.Now()); ok {
			e.RetryAfter = d
		}
	}
	return partial{}, e
}

// grpcTarget is an OTLP/gRPC connection for one signal endpoint.
type grpcTarget struct {
	conn     *grpc.ClientConn
	target   string
	headers  map[string]string
	compress bool
	retry    RetryPolicy
	total    time.Duration
	secrets  []string
	sleep    func(ctx context.Context, d time.Duration) error
}

// grpcHostPort turns an endpoint URL ("http://host:4317") into a dial target.
func grpcHostPort(raw string) (hostport string, secure bool, err error) {
	u, perr := url.Parse(raw)
	if perr != nil || u.Host == "" {
		return "", false, ConfigError("grpc endpoint must be an absolute http or https URL")
	}
	host := u.Host
	if u.Port() == "" {
		host = net.JoinHostPort(u.Hostname(), "4317")
	}
	return host, u.Scheme == "https", nil
}

// newGRPCTarget builds the (lazily connecting) gRPC client of the traces signal
// (used by Ping).
func newGRPCTarget(cfg Config) (*grpcTarget, error) {
	s := SignalTraces
	cfg = cfg.WithDefaults()
	target, secure, err := grpcHostPort(cfg.SignalEndpoint(s))
	if err != nil {
		return nil, err
	}
	creds := insecure.NewCredentials()
	if secure {
		tlsCfg, err := buildTLS(cfg.TLS)
		if err != nil {
			return nil, err
		}
		creds = credentials.NewTLS(tlsCfg)
	}
	ua := UserAgent
	if cfg.UserAgentSuffix != "" {
		ua += " " + cfg.UserAgentSuffix
	}
	conn, err := grpc.NewClient(target, grpc.WithTransportCredentials(creds), grpc.WithUserAgent(ua))
	if err != nil {
		return nil, ConfigError("grpc endpoint is invalid")
	}
	return &grpcTarget{
		conn:     conn,
		target:   target,
		headers:  cfg.SignalHeaders(s),
		compress: cfg.Compression == CompressionGzip,
		retry:    cfg.Retry.withDefaults(),
		total:    cfg.TotalTimeout,
		secrets:  cfg.secretValues(),
		sleep:    sleepContext,
	}, nil
}

func (g *grpcTarget) close() error { return g.conn.Close() }

// invoke runs call with the target's headers and compression, one attempt per
// timeout, retrying UNAVAILABLE with the retry policy's backoff.
func (g *grpcTarget) invoke(ctx context.Context, op string, timeout time.Duration,
	call func(ctx context.Context, conn *grpc.ClientConn, opts []grpc.CallOption) error,
) error {
	if err := ctx.Err(); err != nil {
		return ContextError(op, err)
	}
	var opts []grpc.CallOption
	if g.compress {
		opts = append(opts, grpc.UseCompressor(grpcgzip.Name))
	}
	totalCtx, cancel := context.WithTimeout(ctx, g.total)
	defer cancel()
	if len(g.headers) > 0 {
		totalCtx = metadata.NewOutgoingContext(totalCtx, metadata.New(g.headers))
	}
	for attempt := 0; ; attempt++ {
		actx, acancel := context.WithTimeout(totalCtx, timeout)
		err := call(actx, g.conn, opts)
		acancel()
		if err == nil {
			return nil
		}
		mapped := grpcError(ctx, op, err, g.secrets)
		if mapped.Code != ErrorCodeUnavailable || attempt >= g.retry.MaxRetries {
			return mapped
		}
		if serr := g.sleep(totalCtx, g.retry.Backoff(attempt)); serr != nil {
			return grpcError(ctx, op, err, g.secrets)
		}
	}
}

// grpcError maps a gRPC status to an *Error (OTLP rules: INTERNAL from a malformed
// message is not retryable).
func grpcError(ctx context.Context, op string, err error, secrets []string) *Error {
	if ctx.Err() != nil {
		return contextErr(op, ctx.Err())
	}
	st, _ := grpcstatus.FromError(err)
	var code ErrorCode
	switch st.Code() {
	case codes.Unauthenticated:
		code = ErrorCodeAuthentication
	case codes.PermissionDenied:
		code = ErrorCodePermissionDenied
	case codes.NotFound, codes.Unimplemented:
		code = ErrorCodeNotFound
	case codes.AlreadyExists:
		code = ErrorCodeConflict
	case codes.ResourceExhausted:
		code = ErrorCodeRateLimited
	case codes.DeadlineExceeded:
		code = ErrorCodeTimeout
	case codes.Canceled:
		code = ErrorCodeCancelled
	case codes.InvalidArgument, codes.Internal, codes.FailedPrecondition, codes.OutOfRange:
		code = ErrorCodeValidation
	default: // Unavailable, Aborted, Unknown, DataLoss and unknown codes
		code = ErrorCodeUnavailable
	}
	msg := strings.TrimSpace(RedactText(st.Message(), secrets))
	reason := strings.ToLower(st.Code().String())
	if msg != "" {
		reason += ": " + truncate(msg, maxVendorText)
	}
	return &Error{
		Code:      code,
		Message:   fmt.Sprintf("%s: %s (gRPC %d)", op, reason, int(st.Code())),
		Retryable: code.Retryable(),
		Err:       grpcstatus.Error(st.Code(), msg),
	}
}
