package otelcollector

import (
	"context"
	"strings"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/sdk/resource"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
)

// TracingConfigFromEnv fills every unset field of explicit from the collector
// configuration (ConfigFromEnvWith over env; nil = process environment), see
// TracingConfigFrom. ProtocolNative is a CONFIG error: the collector speaks OTLP only.
func TracingConfigFromEnv(explicit TracingConfig, env map[string]string) (TracingConfig, error) {
	if explicit.Protocol == ProtocolNative {
		return TracingConfig{}, ConfigError("tracing: protocol native is not supported by the OpenTelemetry Collector; use grpc or http/protobuf")
	}
	cfg, err := ConfigFromEnvWith(Config{Protocol: explicit.Protocol}, env)
	if err != nil {
		return TracingConfig{}, err
	}
	return TracingConfigFrom(explicit, cfg), nil
}

// TracingConfigFrom fills every unset field of explicit from the collector config cfg:
// endpoint (an explicit endpoint without a path gets /v1/traces for http/protobuf;
// otherwise TracesURL, URL + /v1/traces or GRPCURL by protocol), protocol (default
// http/protobuf), service name/version/environment, compression, TLS, sampler,
// propagators and batch fields. Headers and resource attributes merge key by key
// (explicit wins).
func TracingConfigFrom(explicit TracingConfig, cfg Config) TracingConfig {
	d := cfg.WithDefaults()
	defaults := d.TracingConfig()
	out := explicit
	if out.Protocol == "" {
		out.Protocol = defaults.Protocol
	}
	switch {
	case explicit.Endpoint != "":
		if out.Protocol != ProtocolGRPC {
			out.Endpoint = otlpHTTPTracesURL(explicit.Endpoint)
		}
	case out.Protocol != defaults.Protocol:
		if out.Protocol == ProtocolGRPC {
			out.Endpoint = d.GRPCURL
		} else {
			out.Endpoint = strings.TrimSuffix(d.URL, "/") + "/v1/traces"
		}
	default:
		out.Endpoint = defaults.Endpoint
	}
	setString := func(dst *string, v string) {
		if strings.TrimSpace(*dst) == "" {
			*dst = v
		}
	}
	setString(&out.ServiceName, defaults.ServiceName)
	setString(&out.ServiceVersion, defaults.ServiceVersion)
	setString(&out.Environment, defaults.Environment)
	setString(&out.Compression, defaults.Compression)
	out.Headers = mergeStrings(defaults.Headers, explicit.Headers)
	out.ResourceAttributes = mergeAny(defaults.ResourceAttributes, explicit.ResourceAttributes)
	if out.Sampler == "" {
		out.Sampler = defaults.Sampler
	}
	if out.SamplerRatio == 0 {
		out.SamplerRatio = defaults.SamplerRatio
	}
	if len(out.Propagators) == 0 {
		out.Propagators = defaults.Propagators
	}
	if out.TLS == (TLSConfig{}) {
		out.TLS = defaults.TLS
	}
	if out.Batch.MaxQueueSize == 0 {
		out.Batch.MaxQueueSize = defaults.Batch.MaxQueueSize
	}
	if out.Batch.MaxExportBatchSize == 0 {
		out.Batch.MaxExportBatchSize = defaults.Batch.MaxExportBatchSize
	}
	if out.Batch.ScheduledDelay == 0 {
		out.Batch.ScheduledDelay = defaults.Batch.ScheduledDelay
	}
	if out.Batch.ExportTimeout == 0 {
		out.Batch.ExportTimeout = defaults.Batch.ExportTimeout
	}
	out.DisableSetGlobal = explicit.DisableSetGlobal || d.DisableSetGlobal
	return out
}

func mergeStrings(base, over map[string]string) map[string]string {
	if len(base) == 0 && len(over) == 0 {
		return over
	}
	out := make(map[string]string, len(base)+len(over))
	for k, v := range base {
		out[k] = v
	}
	for k, v := range over {
		out[k] = v
	}
	return out
}

func mergeAny(base, over map[string]any) map[string]any {
	if len(base) == 0 && len(over) == 0 {
		return over
	}
	out := make(map[string]any, len(base)+len(over))
	for k, v := range base {
		out[k] = v
	}
	for k, v := range over {
		out[k] = v
	}
	return out
}

// NewExporter is the collector ExporterFactory: it resolves cfg with
// TracingConfigFromEnv (process environment) and builds the official OTLP span
// exporter (otlptracehttp or otlptracegrpc).
func NewExporter(ctx context.Context, cfg TracingConfig) (sdktrace.SpanExporter, error) {
	resolved, err := TracingConfigFromEnv(cfg, nil)
	if err != nil {
		return nil, err
	}
	return NewOTLPExporter(ctx, resolved)
}

// TracingOption customizes InitTracing.
type TracingOption func(*tracingOptions)

type tracingOptions struct {
	env       map[string]string
	factory   ExporterFactory
	detectors []string
}

// WithEnv makes InitTracing read env instead of the process environment.
func WithEnv(env map[string]string) TracingOption {
	return func(o *tracingOptions) { o.env = env }
}

// WithExporterFactory replaces the OTLP span exporter (for example with an in-memory
// exporter in tests). The factory receives the resolved config.
func WithExporterFactory(f ExporterFactory) TracingOption {
	return func(o *tracingOptions) { o.factory = f }
}

// WithResourceDetectors selects the resource detectors of InitTracing ("host",
// "process", "container", "k8s"; default all).
func WithResourceDetectors(detectors ...string) TracingOption {
	return func(o *tracingOptions) {
		if detectors == nil {
			detectors = []string{}
		}
		o.detectors = detectors
	}
}

// InitTracing starts OpenTelemetry tracing that exports spans to the collector over
// OTLP (http/protobuf to http://127.0.0.1:4318/v1/traces unless configured otherwise,
// see TracingConfigFromEnv). It builds the official exporter, a batch span processor,
// the sampler, the detected resource (host, OS, process, container, Kubernetes) and
// the propagators, and installs them as the otel globals unless DisableSetGlobal is
// set. Call Shutdown on the handle before exit.
func InitTracing(ctx context.Context, cfg TracingConfig, opts ...TracingOption) (*TracingHandle, error) {
	o := tracingOptions{factory: NewOTLPExporter}
	for _, opt := range opts {
		opt(&o)
	}
	resolved, err := TracingConfigFromEnv(cfg, o.env)
	if err != nil {
		return nil, err
	}
	return startTracing(ctx, resolved, o)
}

// InitTracing starts tracing with the client's configuration as the defaults of cfg
// (see TracingConfigFrom; WithEnv is ignored). Client Close shuts the handle down.
func (c *Client) InitTracing(ctx context.Context, cfg TracingConfig, opts ...TracingOption) (*TracingHandle, error) {
	if cfg.Protocol == ProtocolNative {
		return nil, ConfigError("tracing: protocol native is not supported by the OpenTelemetry Collector; use grpc or http/protobuf")
	}
	o := tracingOptions{factory: NewOTLPExporter, detectors: c.cfg.ResourceDetectors}
	for _, opt := range opts {
		opt(&o)
	}
	h, err := startTracing(ctx, TracingConfigFrom(cfg, c.cfg), o)
	if err != nil {
		return nil, err
	}
	if err := c.register(h.Shutdown); err != nil {
		_ = h.Shutdown(context.Background())
		return nil, err
	}
	return h, nil
}

func startTracing(ctx context.Context, resolved TracingConfig, o tracingOptions) (*TracingHandle, error) {
	if err := resolved.Validate(); err != nil {
		return nil, err
	}
	resolved = resolved.WithDefaults()
	res, err := DetectResource(ctx, Config{ServiceName: resolved.ServiceName, ServiceVersion: resolved.ServiceVersion,
		Environment: resolved.Environment, ResourceAttributes: resolved.ResourceAttributes, ResourceDetectors: o.detectors})
	if err != nil {
		return nil, err
	}
	return newTracingHandle(ctx, resolved, o.factory, res)
}

// newTracingHandle builds a tracer provider for a validated, defaulted config.
func newTracingHandle(ctx context.Context, cfg TracingConfig, factory ExporterFactory, res *resource.Resource) (*TracingHandle, error) {
	exporter, err := factory(ctx, cfg)
	if err != nil {
		return nil, err
	}
	opts := []sdktrace.TracerProviderOption{
		sdktrace.WithSampler(buildSampler(cfg.Sampler, cfg.SamplerRatio)),
		sdktrace.WithBatcher(exporter,
			sdktrace.WithMaxQueueSize(cfg.Batch.MaxQueueSize),
			sdktrace.WithMaxExportBatchSize(cfg.Batch.MaxExportBatchSize),
			sdktrace.WithBatchTimeout(cfg.Batch.ScheduledDelay),
			sdktrace.WithExportTimeout(cfg.Batch.ExportTimeout),
		),
	}
	if res != nil {
		opts = append(opts, sdktrace.WithResource(res))
	}
	provider := sdktrace.NewTracerProvider(opts...)
	h := &TracingHandle{provider: provider, propagator: buildPropagator(cfg.Propagators), config: cfg}
	if !cfg.DisableSetGlobal {
		otel.SetTracerProvider(provider)
		otel.SetTextMapPropagator(h.propagator)
	}
	return h, nil
}
