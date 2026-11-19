package otelcollector

import (
	"context"
	"errors"
	"strings"
	"sync"

	"go.opentelemetry.io/contrib/instrumentation/runtime"
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/exporters/otlp/otlpmetric/otlpmetricgrpc"
	"go.opentelemetry.io/otel/exporters/otlp/otlpmetric/otlpmetrichttp"
	otellog "go.opentelemetry.io/otel/log"
	lognoop "go.opentelemetry.io/otel/log/noop"
	"go.opentelemetry.io/otel/metric"
	metricnoop "go.opentelemetry.io/otel/metric/noop"
	sdklog "go.opentelemetry.io/otel/sdk/log"
	sdkmetric "go.opentelemetry.io/otel/sdk/metric"
	"go.opentelemetry.io/otel/sdk/resource"
	"go.opentelemetry.io/otel/trace"
	tracenoop "go.opentelemetry.io/otel/trace/noop"
	"google.golang.org/grpc/credentials"
)

// TelemetryOption customizes InitTelemetry (mostly for tests).
type TelemetryOption func(*telemetryOptions)

type telemetryOptions struct {
	traceFactory ExporterFactory
	metricReader sdkmetric.Reader
	logExporter  sdklog.Exporter
	logClientExp LogExporter
}

// WithTraceExporterFactory replaces the OTLP span exporter.
func WithTraceExporterFactory(f ExporterFactory) TelemetryOption {
	return func(o *telemetryOptions) { o.traceFactory = f }
}

// WithMetricReader replaces the periodic OTLP metric reader (for example with
// sdkmetric.NewManualReader in tests).
func WithMetricReader(r sdkmetric.Reader) TelemetryOption {
	return func(o *telemetryOptions) { o.metricReader = r }
}

// WithOTelLogExporter replaces the OTLP exporter of the OpenTelemetry LoggerProvider.
func WithOTelLogExporter(e sdklog.Exporter) TelemetryOption {
	return func(o *telemetryOptions) { o.logExporter = e }
}

// WithLogClientExporter replaces the OTLPLogExporter behind TelemetryHandle.LogClient.
func WithLogClientExporter(e LogExporter) TelemetryOption {
	return func(o *telemetryOptions) { o.logClientExp = e }
}

// TelemetryHandle owns the providers created by InitTelemetry: traces (TracerProvider),
// metrics (MeterProvider) and logs (an OpenTelemetry LoggerProvider for OTel log
// bridges plus a LogClient for the package's Logger, slog and zap adapters). All of
// them share one resource and export OTLP to the collector.
type TelemetryHandle struct {
	config         Config
	resource       *resource.Resource
	tracing        *TracingHandle
	meterProvider  *sdkmetric.MeterProvider
	loggerProvider *sdklog.LoggerProvider
	logClient      *BatchingLogClient
	logTransport   *httpTransport

	shutdownOnce sync.Once
	shutdownErr  error
}

// InitTelemetry starts traces, metrics and logs (each unless disabled in cfg) with the
// official OpenTelemetry SDK and OTLP exporters (otlptrace*, otlpmetric*, otlplog*,
// gRPC or HTTP/protobuf per cfg.Protocol), one detected resource (host, OS, process,
// container, Kubernetes, service.*) and, unless DisableSetGlobal is set, installs them
// as the otel globals. Unset fields are NOT read from the environment; use
// InitTelemetryFromEnv for that. Call Shutdown before exit.
func InitTelemetry(ctx context.Context, cfg Config, opts ...TelemetryOption) (*TelemetryHandle, error) {
	if err := cfg.Validate(); err != nil {
		return nil, err
	}
	cfg = cfg.WithDefaults()
	o := telemetryOptions{traceFactory: NewOTLPExporter}
	for _, opt := range opts {
		opt(&o)
	}
	res, err := DetectResource(ctx, cfg)
	if err != nil {
		return nil, err
	}
	h := &TelemetryHandle{config: cfg, resource: res}
	fail := func(err error) (*TelemetryHandle, error) {
		_ = h.Shutdown(context.Background())
		return nil, err
	}
	if !cfg.DisableTraces {
		if h.tracing, err = newTracingHandle(ctx, cfg.TracingConfig(), o.traceFactory, res); err != nil {
			return fail(err)
		}
	}
	if !cfg.DisableMetrics {
		reader := o.metricReader
		if reader == nil {
			exp, err := newMetricExporter(ctx, cfg)
			if err != nil {
				return fail(err)
			}
			reader = sdkmetric.NewPeriodicReader(exp,
				sdkmetric.WithInterval(cfg.MetricExportInterval),
				sdkmetric.WithTimeout(cfg.ExportTimeout))
		}
		h.meterProvider = sdkmetric.NewMeterProvider(sdkmetric.WithResource(res), sdkmetric.WithReader(reader))
		if cfg.RuntimeMetrics {
			if err := runtime.Start(runtime.WithMeterProvider(h.meterProvider)); err != nil {
				return fail(ConfigError("runtime metrics: " + err.Error()))
			}
		}
		if !cfg.DisableSetGlobal {
			otel.SetMeterProvider(h.meterProvider)
		}
	}
	if !cfg.DisableLogs {
		exp := o.logExporter
		if exp == nil {
			t, err := newHTTPTransport(cfg.CommonConfig)
			if err != nil {
				return fail(err)
			}
			h.logTransport = t
			if exp, err = newOTelLogExporterWith(ctx, cfg, t); err != nil {
				return fail(err)
			}
		}
		lb := cfg.LogBatch.withDefaults()
		h.loggerProvider = sdklog.NewLoggerProvider(sdklog.WithResource(res), sdklog.WithProcessor(sdklog.NewBatchProcessor(exp,
			sdklog.WithMaxQueueSize(lb.MaxQueueSize),
			sdklog.WithExportMaxBatchSize(lb.MaxBatchSize),
			sdklog.WithExportInterval(lb.FlushInterval),
			sdklog.WithExportTimeout(lb.ExportTimeout),
		)))
		if !cfg.DisableSetGlobal {
			otel.SetLoggerProvider(h.loggerProvider)
		}
		lexp := o.logClientExp
		if lexp == nil {
			if lexp, err = NewOTLPLogExporter(cfg); err != nil {
				return fail(err)
			}
		}
		if h.logClient, err = NewBatchingLogClient(lexp, LogClientOptions{Resource: resourceAttrMap(res), Batch: cfg.LogBatch}); err != nil {
			return fail(err)
		}
	}
	return h, nil
}

// InitTelemetryFromEnv resolves explicit with ConfigFromEnvWith (process environment)
// and calls InitTelemetry.
func InitTelemetryFromEnv(ctx context.Context, explicit Config, opts ...TelemetryOption) (*TelemetryHandle, error) {
	cfg, err := ConfigFromEnvWith(explicit, nil)
	if err != nil {
		return nil, err
	}
	return InitTelemetry(ctx, cfg, opts...)
}

// InitTelemetry starts telemetry with the client's configuration (see the package
// function InitTelemetry). Client Close shuts the handle down.
func (c *Client) InitTelemetry(ctx context.Context, opts ...TelemetryOption) (*TelemetryHandle, error) {
	h, err := InitTelemetry(ctx, c.cfg, opts...)
	if err != nil {
		return nil, err
	}
	if err := c.register(h.Shutdown); err != nil {
		_ = h.Shutdown(context.Background())
		return nil, err
	}
	return h, nil
}

// resourceAttrMap converts a resource to an attribute map.
func resourceAttrMap(res *resource.Resource) map[string]AttrValue {
	out := map[string]AttrValue{}
	for _, kv := range res.Attributes() {
		out[string(kv.Key)] = kv.Value.AsInterface()
	}
	return out
}

func isSecure(endpoint string) bool { return strings.HasPrefix(endpoint, "https://") }

func newMetricExporter(ctx context.Context, cfg Config) (sdkmetric.Exporter, error) {
	endpoint := cfg.SignalEndpoint(SignalMetrics)
	headers := cfg.SignalHeaders(SignalMetrics)
	tlsCfg, err := buildTLS(cfg.TLS)
	if err != nil {
		return nil, err
	}
	if cfg.Protocol == ProtocolGRPC {
		opts := []otlpmetricgrpc.Option{otlpmetricgrpc.WithEndpointURL(endpoint), otlpmetricgrpc.WithTimeout(cfg.ExportTimeout)}
		if len(headers) > 0 {
			opts = append(opts, otlpmetricgrpc.WithHeaders(headers))
		}
		if cfg.Compression == CompressionGzip {
			opts = append(opts, otlpmetricgrpc.WithCompressor("gzip"))
		}
		if isSecure(endpoint) {
			opts = append(opts, otlpmetricgrpc.WithTLSCredentials(credentials.NewTLS(tlsCfg)))
		} else {
			opts = append(opts, otlpmetricgrpc.WithInsecure())
		}
		return otlpmetricgrpc.New(ctx, opts...)
	}
	opts := []otlpmetrichttp.Option{otlpmetrichttp.WithEndpointURL(endpoint), otlpmetrichttp.WithTimeout(cfg.ExportTimeout)}
	if len(headers) > 0 {
		opts = append(opts, otlpmetrichttp.WithHeaders(headers))
	}
	if cfg.Compression == CompressionGzip {
		opts = append(opts, otlpmetrichttp.WithCompression(otlpmetrichttp.GzipCompression))
	} else {
		opts = append(opts, otlpmetrichttp.WithCompression(otlpmetrichttp.NoCompression))
	}
	if isSecure(endpoint) {
		opts = append(opts, otlpmetrichttp.WithTLSClientConfig(tlsCfg))
	}
	return otlpmetrichttp.New(ctx, opts...)
}

// Config returns the defaulted configuration the handle was built with.
func (h *TelemetryHandle) Config() Config { return h.config }

// Resource returns the shared resource.
func (h *TelemetryHandle) Resource() *resource.Resource { return h.resource }

// Tracing returns the tracing handle, nil when traces are disabled.
func (h *TelemetryHandle) Tracing() *TracingHandle { return h.tracing }

// TracerProvider returns the tracer provider (a no-op provider when traces are disabled).
func (h *TelemetryHandle) TracerProvider() trace.TracerProvider {
	if h.tracing == nil {
		return tracenoop.NewTracerProvider()
	}
	return h.tracing.TracerProvider()
}

// Tracer returns a named tracer.
func (h *TelemetryHandle) Tracer(name string, opts ...trace.TracerOption) trace.Tracer {
	return h.TracerProvider().Tracer(name, opts...)
}

// MeterProvider returns the meter provider (a no-op provider when metrics are disabled).
func (h *TelemetryHandle) MeterProvider() metric.MeterProvider {
	if h.meterProvider == nil {
		return metricnoop.NewMeterProvider()
	}
	return h.meterProvider
}

// Meter returns a named meter (an empty name means DefaultMeterName).
func (h *TelemetryHandle) Meter(name string, opts ...metric.MeterOption) metric.Meter {
	if name == "" {
		name = DefaultMeterName
	}
	return h.MeterProvider().Meter(name, opts...)
}

// LoggerProvider returns the OpenTelemetry logger provider for OTel log bridges such as
// otelslog (a no-op provider when logs are disabled).
func (h *TelemetryHandle) LoggerProvider() otellog.LoggerProvider {
	if h.loggerProvider == nil {
		return lognoop.NewLoggerProvider()
	}
	return h.loggerProvider
}

// LogClient returns the LogClient that exports OTLP logs with the shared resource (nil
// when logs are disabled). Use it with Logger, NewSlogHandler or zapadapter.
func (h *TelemetryHandle) LogClient() LogClient {
	if h.logClient == nil {
		return nil
	}
	return h.logClient
}

// Logger returns a Logger named name on LogClient (records are dropped when logs are
// disabled).
func (h *TelemetryHandle) Logger(name string, attrs map[string]AttrValue) *Logger {
	if h.logClient == nil {
		return NewLogger(noopLogClient{}, LoggerOptions{Name: name, Attributes: attrs})
	}
	return h.logClient.Logger(name, attrs)
}

// DefaultMeterName is the meter of the instrument helpers when InstrumentOptions.Meter
// is empty.
const DefaultMeterName = InstrumentationScope

// InstrumentOptions describe a metric instrument. The zero value is valid.
type InstrumentOptions struct {
	// Description is the instrument description.
	Description string
	// Unit is the UCUM unit ("ms", "By", "{request}").
	Unit string
	// Meter is the meter to create the instrument on (default DefaultMeterName).
	Meter string
	// Boundaries are explicit histogram bucket boundaries, ascending (histograms only;
	// empty = the SDK default).
	Boundaries []float64
}

// instruments creates instruments on meters of one provider.
type instruments func() metric.MeterProvider

func (p instruments) meter(o InstrumentOptions) metric.Meter {
	name := o.Meter
	if name == "" {
		name = DefaultMeterName
	}
	return p().Meter(name, metric.WithInstrumentationVersion(Version))
}

func (p instruments) counter(name string, o InstrumentOptions) (metric.Int64Counter, error) {
	c, err := p.meter(o).Int64Counter(name, metric.WithDescription(o.Description), metric.WithUnit(o.Unit))
	if err != nil {
		return nil, ValidationError("metrics.counter", err.Error())
	}
	return c, nil
}

func (p instruments) upDownCounter(name string, o InstrumentOptions) (metric.Int64UpDownCounter, error) {
	c, err := p.meter(o).Int64UpDownCounter(name, metric.WithDescription(o.Description), metric.WithUnit(o.Unit))
	if err != nil {
		return nil, ValidationError("metrics.upDownCounter", err.Error())
	}
	return c, nil
}

func (p instruments) histogram(name string, o InstrumentOptions) (metric.Float64Histogram, error) {
	opts := []metric.Float64HistogramOption{metric.WithDescription(o.Description), metric.WithUnit(o.Unit)}
	if len(o.Boundaries) > 0 {
		opts = append(opts, metric.WithExplicitBucketBoundaries(o.Boundaries...))
	}
	h, err := p.meter(o).Float64Histogram(name, opts...)
	if err != nil {
		return nil, ValidationError("metrics.histogram", err.Error())
	}
	return h, nil
}

func (p instruments) gauge(name string, o InstrumentOptions) (metric.Float64Gauge, error) {
	g, err := p.meter(o).Float64Gauge(name, metric.WithDescription(o.Description), metric.WithUnit(o.Unit))
	if err != nil {
		return nil, ValidationError("metrics.gauge", err.Error())
	}
	return g, nil
}

func (h *TelemetryHandle) instruments() instruments { return h.MeterProvider }

// Counter creates a monotonic int64 counter (OTLP sum, cumulative).
func (h *TelemetryHandle) Counter(name string, opts InstrumentOptions) (metric.Int64Counter, error) {
	return h.instruments().counter(name, opts)
}

// UpDownCounter creates a non-monotonic int64 counter.
func (h *TelemetryHandle) UpDownCounter(name string, opts InstrumentOptions) (metric.Int64UpDownCounter, error) {
	return h.instruments().upDownCounter(name, opts)
}

// Histogram creates a float64 histogram with opts.Boundaries as explicit buckets.
func (h *TelemetryHandle) Histogram(name string, opts InstrumentOptions) (metric.Float64Histogram, error) {
	return h.instruments().histogram(name, opts)
}

// Gauge creates a synchronous float64 gauge (last value).
func (h *TelemetryHandle) Gauge(name string, opts InstrumentOptions) (metric.Float64Gauge, error) {
	return h.instruments().gauge(name, opts)
}

var globalInstruments instruments = otel.GetMeterProvider

// Counter creates a monotonic int64 counter on the global meter provider (set by
// InitTelemetry unless DisableSetGlobal).
func Counter(name string, opts InstrumentOptions) (metric.Int64Counter, error) {
	return globalInstruments.counter(name, opts)
}

// UpDownCounter creates a non-monotonic int64 counter on the global meter provider.
func UpDownCounter(name string, opts InstrumentOptions) (metric.Int64UpDownCounter, error) {
	return globalInstruments.upDownCounter(name, opts)
}

// Histogram creates a float64 histogram on the global meter provider.
func Histogram(name string, opts InstrumentOptions) (metric.Float64Histogram, error) {
	return globalInstruments.histogram(name, opts)
}

// Gauge creates a synchronous float64 gauge on the global meter provider.
func Gauge(name string, opts InstrumentOptions) (metric.Float64Gauge, error) {
	return globalInstruments.gauge(name, opts)
}

// ForceFlush exports all ended spans, collected metrics and queued log records now.
func (h *TelemetryHandle) ForceFlush(ctx context.Context) error {
	var errs []error
	if h.tracing != nil {
		errs = append(errs, h.tracing.ForceFlush(ctx))
	}
	if h.meterProvider != nil {
		errs = append(errs, h.meterProvider.ForceFlush(ctx))
	}
	if h.loggerProvider != nil {
		errs = append(errs, h.loggerProvider.ForceFlush(ctx))
	}
	if h.logClient != nil {
		errs = append(errs, h.logClient.Flush(ctx))
	}
	return wrapTelemetryErr("telemetry.flush", errors.Join(errs...))
}

// Shutdown flushes and stops every provider. It is idempotent: later calls return the
// first result.
func (h *TelemetryHandle) Shutdown(ctx context.Context) error {
	h.shutdownOnce.Do(func() {
		var errs []error
		if h.logClient != nil {
			errs = append(errs, h.logClient.Close(ctx))
		}
		if h.loggerProvider != nil {
			errs = append(errs, h.loggerProvider.Shutdown(ctx))
		}
		if h.logTransport != nil {
			h.logTransport.close()
		}
		if h.meterProvider != nil {
			errs = append(errs, h.meterProvider.Shutdown(ctx))
		}
		if h.tracing != nil {
			errs = append(errs, h.tracing.Shutdown(ctx))
		}
		h.shutdownErr = wrapTelemetryErr("telemetry.shutdown", errors.Join(errs...))
	})
	return h.shutdownErr
}

func wrapTelemetryErr(op string, err error) error {
	if err == nil {
		return nil
	}
	var e *Error
	if errors.As(err, &e) {
		return e
	}
	return ContextError(op, err)
}

// Meter returns a named meter of the global meter provider (set by InitTelemetry);
// an empty name means DefaultMeterName.
func Meter(name string, opts ...metric.MeterOption) metric.Meter {
	if name == "" {
		name = DefaultMeterName
	}
	return otel.GetMeterProvider().Meter(name, opts...)
}

// noopLogClient drops every record.
type noopLogClient struct{}

func (noopLogClient) Send(context.Context, LogEntry)        {}
func (noopLogClient) SendBatch(context.Context, []LogEntry) {}
func (noopLogClient) Flush(context.Context) error           { return nil }
func (noopLogClient) Close(context.Context) error           { return nil }
func (noopLogClient) Stats() BatchStats                     { return BatchStats{} }
func (n noopLogClient) Logger(name string, attrs map[string]AttrValue) *Logger {
	return NewLogger(n, LoggerOptions{Name: name, Attributes: attrs})
}
