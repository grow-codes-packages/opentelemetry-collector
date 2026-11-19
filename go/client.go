package otelcollector

import (
	"context"
	"errors"
	"net/http"
	"net/url"
	"regexp"
	"strings"
	"sync"
	"time"

	coltracepb "go.opentelemetry.io/proto/otlp/collector/trace/v1"
	"google.golang.org/grpc"
)

// Client is the OpenTelemetry Collector client: health (health_check with a zpages
// fallback), Ping (an empty OTLP export), ServerVersion (zpages), the OTLP log
// exporter and LogClient, InitTelemetry and the (unsupported) query clients. It is
// safe for concurrent use.
type Client struct {
	cfg       Config
	transport *httpTransport
	traces    *endpoint
	health    *endpoint
	zpages    *endpoint
	telemetry *endpoint
	grpc      *grpcTarget

	mu      sync.Mutex
	closers []func(context.Context) error
	closed  bool

	closeOnce sync.Once
}

// endpoint is a transport bound to an absolute URL.
type endpoint struct {
	t    *httpTransport
	url  string
	path string
}

// newEndpoint builds a transport for the absolute URL raw with the common settings of cfg.
func newEndpoint(cfg CommonConfig, raw string, secrets []string) (*endpoint, error) {
	u, err := url.Parse(raw)
	if err != nil || u.Host == "" {
		return nil, ConfigError("endpoint url is invalid")
	}
	cfg.URL = u.Scheme + "://" + u.Host
	t, err := newHTTPTransport(cfg)
	if err != nil {
		return nil, err
	}
	t.addSecret(secrets...)
	path := u.Path
	if path == "" {
		path = "/"
	}
	return &endpoint{t: t, url: raw, path: path}, nil
}

// sub returns a request for path below the endpoint path.
func (e *endpoint) get(operation, sub string, accept string) *request {
	p := e.path
	if sub != "" {
		p = strings.TrimSuffix(p, "/") + sub
	}
	return &request{Method: http.MethodGet, Path: p, Operation: operation, Header: http.Header{"Accept": {accept}}}
}

func (e *endpoint) close() {
	if e != nil {
		e.t.close()
	}
}

// NewClient validates cfg and returns a client. It makes no network call.
func NewClient(cfg Config) (*Client, error) {
	if err := cfg.Validate(); err != nil {
		return nil, err
	}
	cfg = cfg.WithDefaults()
	secrets := cfg.secretValues()
	t, err := newHTTPTransport(cfg.CommonConfig)
	if err != nil {
		return nil, err
	}
	t.addSecret(secrets...)
	c := &Client{cfg: cfg, transport: t}
	if cfg.Protocol == ProtocolGRPC {
		if c.grpc, err = newGRPCTarget(cfg); err != nil {
			return nil, err
		}
	} else if c.traces, err = newEndpoint(cfg.CommonConfig, cfg.SignalEndpoint(SignalTraces), secrets); err != nil {
		return nil, err
	}
	if c.health, err = newEndpoint(cfg.CommonConfig, cfg.HealthURL, secrets); err != nil {
		return nil, err
	}
	if c.zpages, err = newEndpoint(cfg.CommonConfig, cfg.ZPagesURL, secrets); err != nil {
		return nil, err
	}
	if cfg.TelemetryURL != "" {
		if c.telemetry, err = newEndpoint(cfg.CommonConfig, cfg.TelemetryURL, secrets); err != nil {
			return nil, err
		}
	}
	return c, nil
}

// NewClientFromEnv builds a client from the environment (see ConfigFromEnv).
func NewClientFromEnv() (*Client, error) {
	cfg, err := ConfigFromEnv()
	if err != nil {
		return nil, err
	}
	return NewClient(cfg)
}

// Config returns the effective configuration (defaults applied).
func (c *Client) Config() Config { return c.cfg }

// Health checks the collector without credentials: GET HealthURL (the health_check
// extension, port 13133). 200 means healthy (Status "ok", Details vendorStatus,
// upSince, uptime); 503 "Server not available" means Status "unhealthy". When the
// health_check endpoint is unreachable (stock stacks bind it to localhost) Health falls
// back to GET ZPagesURL/debug/servicez: 200 → healthy with Details source=zpages.
// It reports an unreachable or unhealthy collector in the result; it fails only for
// CANCELLED and CONFIG.
func (c *Client) Health(ctx context.Context) (HealthStatus, error) {
	return c.CollectorHealth().Health(ctx)
}

// CollectorHealth returns the health client of this collector.
func (c *Client) CollectorHealth() *CollectorHealthClient {
	return &CollectorHealthClient{health: c.health, zpages: c.zpages, closed: c.isClosed}
}

// WaitUntilReady polls Health until the collector is healthy (see
// CollectorHealthClient.WaitUntilReady).
func (c *Client) WaitUntilReady(ctx context.Context, timeout time.Duration) (HealthStatus, error) {
	return c.CollectorHealth().WaitUntilReady(ctx, timeout)
}

func (c *Client) isClosed() bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.closed
}

// Ping makes an authenticated round trip and returns its latency: an empty OTLP trace
// export (POST <traces endpoint> with an empty protobuf body, or TraceService/Export
// with an empty message over gRPC). The collector stores nothing for it.
// AUTHENTICATION / PERMISSION_DENIED when an authenticator or gateway rejects Headers.
func (c *Client) Ping(ctx context.Context) (time.Duration, error) {
	const op = "ping"
	if c.isClosed() {
		return 0, &Error{Code: ErrorCodeCancelled, Message: op + ": client is closed"}
	}
	start := time.Now()
	if c.grpc != nil {
		err := c.grpc.invoke(ctx, op, c.cfg.RequestTimeout, func(cctx context.Context, conn *grpc.ClientConn, opts []grpc.CallOption) error {
			_, err := coltracepb.NewTraceServiceClient(conn).Export(cctx, &coltracepb.ExportTraceServiceRequest{}, opts...)
			return err
		})
		if err != nil {
			return 0, err
		}
		return time.Since(start), nil
	}
	_, err := postOTLP(ctx, c.traces, op, []byte{})
	if err != nil {
		return 0, err
	}
	return time.Since(start), nil
}

var (
	servicezVersionRe = regexp.MustCompile(`<b>Version</b></td>\s*<td>[^<]*</td>\s*<td[^>]*>([^<]+)</td>`)
	targetInfoRe      = regexp.MustCompile(`target_info\{[^}]*service_version="([^"]+)"`)
)

// ServerVersion returns the collector version (for example "0.165.0"). It parses the
// Version row of the zpages page ZPagesURL/debug/servicez (reachable on stock stacks)
// and falls back to the target_info metric of TelemetryURL. UNSUPPORTED when neither
// is reachable or carries a version. The version is never derived from OTLP
// responses or the image tag.
func (c *Client) ServerVersion(ctx context.Context) (string, error) {
	const op = "serverVersion"
	if c.isClosed() {
		return "", &Error{Code: ErrorCodeCancelled, Message: op + ": client is closed"}
	}
	res, err := c.zpages.t.do(ctx, c.zpages.get(op, "/debug/servicez", "text/html"))
	if err == nil {
		if m := servicezVersionRe.FindSubmatch(res.Body); m != nil {
			return strings.TrimPrefix(strings.TrimSpace(string(m[1])), "v"), nil
		}
	} else if fatal := fatalProbeError(ctx, err); fatal != nil {
		return "", fatal
	}
	if c.telemetry != nil {
		mres, merr := c.telemetry.t.do(ctx, c.telemetry.get(op, "", "text/plain"))
		if merr == nil {
			if m := targetInfoRe.FindSubmatch(mres.Body); m != nil {
				return strings.TrimPrefix(string(m[1]), "v"), nil
			}
		} else if fatal := fatalProbeError(ctx, merr); fatal != nil {
			return "", fatal
		}
	}
	return "", UnsupportedError(op, "neither zpages nor the telemetry endpoint exposes a version")
}

// fatalProbeError returns err when a version probe must not fall back: caller
// cancellation, a closed client and authentication failures.
func fatalProbeError(ctx context.Context, err error) error {
	if ctx.Err() != nil {
		return err
	}
	switch CodeOf(err) {
	case ErrorCodeCancelled, ErrorCodeAuthentication, ErrorCodePermissionDenied:
		return err
	}
	return nil
}

// TraceQuery returns the trace query client. The collector stores nothing, so every
// method returns UNSUPPORTED (read traces from the backend the collector exports to).
func (c *Client) TraceQuery() TraceQueryClient { return UnsupportedTraceQueryClient{} }

// LogQuery returns the log query client. The collector stores nothing, so every method
// returns UNSUPPORTED.
func (c *Client) LogQuery() LogQueryClient { return UnsupportedLogQueryClient{} }

// register adds a closer run by Close (newest first).
func (c *Client) register(fn func(context.Context) error) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return &Error{Code: ErrorCodeCancelled, Message: "client is closed"}
	}
	c.closers = append(c.closers, fn)
	return nil
}

// Close flushes and shuts down every LogClient, exporter and telemetry handle created
// through the client and releases its connections. It is idempotent.
func (c *Client) Close() error {
	var errs []error
	c.closeOnce.Do(func() {
		c.mu.Lock()
		c.closed = true
		closers := c.closers
		c.closers = nil
		c.mu.Unlock()
		ctx, cancel := context.WithTimeout(context.Background(), c.cfg.ExportTimeout+5*time.Second)
		defer cancel()
		for i := len(closers) - 1; i >= 0; i-- {
			if err := closers[i](ctx); err != nil {
				errs = append(errs, err)
			}
		}
		c.transport.close()
		c.traces.close()
		c.health.close()
		c.zpages.close()
		c.telemetry.close()
		if c.grpc != nil {
			_ = c.grpc.close()
		}
	})
	return errors.Join(errs...)
}
