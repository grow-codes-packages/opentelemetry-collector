package otelcollector

import (
	"fmt"
	"net"
	"net/url"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"
)

// Collector defaults (stack catalog ports).
const (
	// DefaultURL is the default OTLP/HTTP base URL (port 4318).
	DefaultURL = "http://127.0.0.1:4318"
	// DefaultGRPCURL is the default OTLP/gRPC target (port 4317).
	DefaultGRPCURL = "http://127.0.0.1:4317"
	// DefaultHealthPort is the health_check extension port.
	DefaultHealthPort = "13133"
	// DefaultZPagesPort is the zpages extension port.
	DefaultZPagesPort = "55679"
	// DefaultProtocol is the default export protocol (OTLP over HTTP/protobuf).
	DefaultProtocol = ProtocolHTTPProtobuf
	// DefaultOTLPExportTimeout bounds one OTLP export (OpenTelemetry default 10 s).
	DefaultOTLPExportTimeout = 10 * time.Second
	// DefaultMetricExportInterval is the periodic metric reader interval (60 s).
	DefaultMetricExportInterval = 60 * time.Second
)

// Signal is an OpenTelemetry signal.
type Signal string

// Signals.
const (
	SignalTraces  Signal = "traces"
	SignalLogs    Signal = "logs"
	SignalMetrics Signal = "metrics"
)

// OTelBatchOptions configure an OpenTelemetry batch processor (spans or log records).
// Zero values mean defaults.
type OTelBatchOptions = TracingBatchOptions

// Config configures a Client and InitTelemetry. Build it with a struct literal or
// ConfigFromEnv; zero values mean "use the default". Call Validate to check it.
//
// CommonConfig.URL is the OTLP/HTTP base URL (default http://127.0.0.1:4318);
// CommonConfig.Headers are sent with every export and with Ping (put gateway
// credentials here, they are redacted everywhere).
//
// Environment variables read by ConfigFromEnv (explicit value >
// GROW_OPENTELEMETRY_COLLECTOR_* > GROW_OTEL_* > per-signal OTEL_* > generic OTEL_* >
// default):
//
//	URL                 URL, OTEL_EXPORTER_OTLP_ENDPOINT (protocol http/protobuf only)
//	GRPCURL             GRPC_URL, OTEL_EXPORTER_OTLP_ENDPOINT (protocol grpc only)
//	Protocol            PROTOCOL, OTEL_EXPORTER_OTLP_PROTOCOL (grpc | http/protobuf)
//	TracesURL ...       TRACES_URL, LOGS_URL, METRICS_URL, OTEL_EXPORTER_OTLP_<SIGNAL>_ENDPOINT (used as is)
//	Headers             HEADERS, OTEL_EXPORTER_OTLP_HEADERS (k=v,k2=v2; URL-encoded values)
//	TracesHeaders ...   OTEL_EXPORTER_OTLP_<SIGNAL>_HEADERS (merged over Headers)
//	Compression         COMPRESSION, OTEL_EXPORTER_OTLP_COMPRESSION (gzip | none)
//	ExportTimeout       EXPORT_TIMEOUT, OTEL_EXPORTER_OTLP_TIMEOUT (integer ms)
//	TLS                 CA_CERT, CLIENT_CERT, CLIENT_KEY, OTEL_EXPORTER_OTLP_CERTIFICATE,
//	                    OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE, OTEL_EXPORTER_OTLP_CLIENT_KEY
//	HealthURL           HEALTH_URL (default URL host, port 13133, path /)
//	ZPagesURL           ZPAGES_URL (default URL host, port 55679)
//	TelemetryURL        TELEMETRY_URL (optional, the collector's :8888/metrics)
//	ServiceName         SERVICE_NAME, OTEL_SERVICE_NAME, service.name in OTEL_RESOURCE_ATTRIBUTES
//	ServiceVersion      SERVICE_VERSION
//	Environment         ENVIRONMENT
//	ResourceAttributes  RESOURCE_ATTRIBUTES, OTEL_RESOURCE_ATTRIBUTES (explicit keys win)
//	Disable*            TRACES, LOGS, METRICS (bool), OTEL_<SIGNAL>_EXPORTER=none, OTEL_SDK_DISABLED
//	Sampler             SAMPLER, OTEL_TRACES_SAMPLER; SAMPLER_RATIO, OTEL_TRACES_SAMPLER_ARG
//	Propagators         PROPAGATORS, OTEL_PROPAGATORS
//	TraceBatch          OTEL_BSP_SCHEDULE_DELAY, OTEL_BSP_EXPORT_TIMEOUT, OTEL_BSP_MAX_QUEUE_SIZE, OTEL_BSP_MAX_EXPORT_BATCH_SIZE
//	LogBatch            OTEL_BLRP_SCHEDULE_DELAY, OTEL_BLRP_EXPORT_TIMEOUT, OTEL_BLRP_MAX_QUEUE_SIZE, OTEL_BLRP_MAX_EXPORT_BATCH_SIZE
//	MetricExportInterval METRIC_EXPORT_INTERVAL, OTEL_METRIC_EXPORT_INTERVAL (ms)
//	ResourceDetectors   RESOURCE_DETECTORS (host,process,container,k8s or none)
//	RuntimeMetrics      RUNTIME_METRICS (bool)
//
// plus the common CONNECT_TIMEOUT, TOTAL_TIMEOUT, MAX_RETRIES, INSECURE_SKIP_VERIFY and PROXY.
type Config struct {
	CommonConfig

	// GRPCURL is the OTLP/gRPC target, for example http://collector:4317 (https:// = TLS).
	GRPCURL string
	// Protocol is the export protocol: ProtocolHTTPProtobuf (default) or ProtocolGRPC.
	// ProtocolNative is not supported by the collector (CONFIG error).
	Protocol Protocol
	// TracesURL, LogsURL and MetricsURL override the endpoint of one signal and are
	// used as is (HTTP: no path is appended; gRPC: the target).
	TracesURL  string
	LogsURL    string
	MetricsURL string
	// TracesHeaders, LogsHeaders and MetricsHeaders are added to Headers for one signal
	// (signal values win per key).
	TracesHeaders  map[string]string
	LogsHeaders    map[string]string
	MetricsHeaders map[string]string
	// Compression is "gzip" (default) or "none".
	Compression string
	// ExportTimeout bounds one OTLP export (default 10 s).
	ExportTimeout time.Duration

	// HealthURL is the health_check extension URL (default: URL host, port 13133, "/").
	HealthURL string
	// ZPagesURL is the zpages extension base URL (default: URL host, port 55679).
	ZPagesURL string
	// TelemetryURL is the collector's own Prometheus endpoint (:8888/metrics),
	// optional; ServerVersion falls back to its target_info metric.
	TelemetryURL string

	// ServiceName is the service.name resource attribute (default
	// "unknown_service:<process name>").
	ServiceName string
	// ServiceVersion is the service.version resource attribute.
	ServiceVersion string
	// Environment is the deployment.environment.name (and deployment.environment)
	// resource attribute.
	Environment string
	// ResourceAttributes are extra resource attributes (AttrValue kinds).
	ResourceAttributes map[string]any
	// ResourceDetectors are the resource detectors of InitTelemetry and InitTracing:
	// "host", "process", "container", "k8s" (nil = all; empty non-nil = none).
	ResourceDetectors []string

	// DisableTraces, DisableLogs and DisableMetrics turn a signal off in InitTelemetry.
	DisableTraces  bool
	DisableLogs    bool
	DisableMetrics bool

	// Sampler is the head sampler (default parentbased_always_on).
	Sampler Sampler
	// SamplerRatio is the ratio of the *traceidratio samplers (0 = default 1.0).
	SamplerRatio float64
	// Propagators are propagator names (default tracecontext, baggage).
	Propagators []string
	// TraceBatch configures the span batch processor (SPEC §5 defaults).
	TraceBatch OTelBatchOptions
	// LogBatch configures the log batch processor of LogClient (and the OpenTelemetry
	// LoggerProvider of InitTelemetry); zero fields mean the SPEC §4 defaults.
	LogBatch BatchOptions
	// MetricExportInterval is the periodic metric export interval (default 60 s).
	MetricExportInterval time.Duration
	// RuntimeMetrics adds Go runtime metrics (go.opentelemetry.io/contrib runtime).
	RuntimeMetrics bool
	// DisableSetGlobal keeps the providers and the propagator out of the otel globals.
	DisableSetGlobal bool
}

// aliasPrefix is the short environment prefix accepted after GROW_OPENTELEMETRY_COLLECTOR_.
const aliasPrefix = "GROW_OTEL_"

// ConfigFromEnv reads the configuration from the process environment.
func ConfigFromEnv() (Config, error) { return ConfigFromEnvWith(Config{}, nil) }

// envResolver reads GROW_OPENTELEMETRY_COLLECTOR_<NAME>, then GROW_OTEL_<NAME>, then
// the vendor variables.
type envResolver struct{ r *EnvReader }

func (e envResolver) names(name string, vendor []string) []string {
	out := make([]string, 0, len(vendor)+1)
	if name != "" {
		out = append(out, aliasPrefix+name)
	}
	return append(out, vendor...)
}

func (e envResolver) find(name string, vendor ...string) (value, variable string, ok bool) {
	return e.r.find(name, e.names(name, vendor))
}

func (e envResolver) str(name string, vendor ...string) (string, bool) {
	v, _, ok := e.find(name, vendor...)
	return v, ok
}

func (e envResolver) duration(name string, vendor ...string) (time.Duration, bool, error) {
	return e.r.Duration(name, e.names(name, vendor)...)
}

func (e envResolver) boolean(name string, vendor ...string) (value, ok bool, err error) {
	return e.r.Bool(name, e.names(name, vendor)...)
}

// ConfigFromEnvWith resolves explicit values > GROW_OPENTELEMETRY_COLLECTOR_* >
// GROW_OTEL_* > OTEL_* variables > defaults (see Config). env replaces the process
// environment when non-nil. The result has every default applied and is validated.
func ConfigFromEnvWith(explicit Config, env map[string]string) (Config, error) {
	e := envResolver{r: NewEnvReader(EnvPrefix, env)}
	c := explicit
	if c.Protocol == "" {
		if v, variable, ok := e.find("PROTOCOL", "OTEL_EXPORTER_OTLP_PROTOCOL"); ok {
			p, err := parseProtocol(v)
			if err != nil {
				return Config{}, ConfigError(variable + " must be grpc or http/protobuf")
			}
			c.Protocol = p
		}
	}
	grpcMode := c.Protocol == ProtocolGRPC
	vendor := VendorEnv{
		URL:                []string{aliasPrefix + "URL"},
		CACert:             []string{aliasPrefix + "CA_CERT", "OTEL_EXPORTER_OTLP_CERTIFICATE"},
		ClientCert:         []string{aliasPrefix + "CLIENT_CERT", "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE"},
		ClientKey:          []string{aliasPrefix + "CLIENT_KEY", "OTEL_EXPORTER_OTLP_CLIENT_KEY"},
		InsecureSkipVerify: []string{aliasPrefix + "INSECURE_SKIP_VERIFY"},
		Proxy:              []string{aliasPrefix + "PROXY"},
	}
	if !grpcMode {
		vendor.URL = append(vendor.URL, "OTEL_EXPORTER_OTLP_ENDPOINT")
	}
	if err := c.applyEnv(e.r, vendor); err != nil {
		return Config{}, err
	}
	if err := c.applyOTLPEnv(e, grpcMode); err != nil {
		return Config{}, err
	}
	if err := c.applyResourceEnv(e); err != nil {
		return Config{}, err
	}
	if err := c.applySignalEnv(e); err != nil {
		return Config{}, err
	}
	if err := c.applyTracingEnv(e); err != nil {
		return Config{}, err
	}
	if err := c.applyBatchEnv(e); err != nil {
		return Config{}, err
	}
	c = c.WithDefaults()
	if err := c.Validate(); err != nil {
		return Config{}, err
	}
	return c, nil
}

// applyOTLPEnv fills the exporter connection fields.
func (c *Config) applyOTLPEnv(e envResolver, grpcMode bool) error {
	if c.GRPCURL == "" {
		vendor := []string{}
		if grpcMode {
			vendor = append(vendor, "OTEL_EXPORTER_OTLP_ENDPOINT")
		}
		c.GRPCURL, _ = e.str("GRPC_URL", vendor...)
	}
	for _, s := range []struct {
		dst  *string
		name string
	}{{&c.TracesURL, "TRACES"}, {&c.LogsURL, "LOGS"}, {&c.MetricsURL, "METRICS"}} {
		if *s.dst == "" {
			*s.dst, _ = e.str(s.name+"_URL", "OTEL_EXPORTER_OTLP_"+s.name+"_ENDPOINT")
		}
	}
	if len(c.Headers) == 0 {
		v, variable, ok := e.find("HEADERS")
		if ok {
			h, err := parseKeyValueList(v)
			if err != nil {
				return ConfigError(variable + " must be a list of key=value pairs")
			}
			c.Headers = h
		} else if v, variable, ok := e.r.find("", []string{"OTEL_EXPORTER_OTLP_HEADERS"}); ok {
			h, err := parseKeyValueList(v)
			if err != nil {
				return ConfigError(variable + " must be a list of key=value pairs")
			}
			c.Headers = h
		}
	}
	for _, s := range []struct {
		dst  *map[string]string
		name string
	}{{&c.TracesHeaders, "TRACES"}, {&c.LogsHeaders, "LOGS"}, {&c.MetricsHeaders, "METRICS"}} {
		if len(*s.dst) > 0 {
			continue
		}
		if v, variable, ok := e.r.find("", []string{"OTEL_EXPORTER_OTLP_" + s.name + "_HEADERS"}); ok {
			h, err := parseKeyValueList(v)
			if err != nil {
				return ConfigError(variable + " must be a list of key=value pairs")
			}
			*s.dst = h
		}
	}
	if c.Compression == "" {
		if v, variable, ok := e.find("COMPRESSION", "OTEL_EXPORTER_OTLP_COMPRESSION"); ok {
			switch strings.ToLower(strings.TrimSpace(v)) {
			case CompressionGzip:
				c.Compression = CompressionGzip
			case CompressionNone:
				c.Compression = CompressionNone
			default:
				return ConfigError(variable + " must be gzip or none")
			}
		}
	}
	if c.ExportTimeout == 0 {
		d, ok, err := e.duration("EXPORT_TIMEOUT", "OTEL_EXPORTER_OTLP_TIMEOUT")
		if err != nil {
			return err
		}
		if ok {
			c.ExportTimeout = d
		}
	}
	for _, s := range []struct {
		dst  *string
		name string
	}{{&c.HealthURL, "HEALTH_URL"}, {&c.ZPagesURL, "ZPAGES_URL"}, {&c.TelemetryURL, "TELEMETRY_URL"}} {
		if *s.dst == "" {
			*s.dst, _ = e.str(s.name)
		}
	}
	return nil
}

// applyResourceEnv fills ServiceName and ResourceAttributes.
func (c *Config) applyResourceEnv(e envResolver) error {
	var envAttrs map[string]string
	if v, variable, ok := e.find("RESOURCE_ATTRIBUTES", "OTEL_RESOURCE_ATTRIBUTES"); ok {
		attrs, err := parseKeyValueList(v)
		if err != nil {
			return ConfigError(variable + " must be a list of key=value pairs")
		}
		envAttrs = attrs
	}
	if c.ServiceName == "" {
		c.ServiceName, _ = e.str("SERVICE_NAME", "OTEL_SERVICE_NAME")
	}
	if c.ServiceVersion == "" {
		c.ServiceVersion, _ = e.str("SERVICE_VERSION")
	}
	if c.Environment == "" {
		c.Environment, _ = e.str("ENVIRONMENT")
	}
	if c.ResourceDetectors == nil {
		if v, variable, ok := e.find("RESOURCE_DETECTORS"); ok {
			d, err := parseDetectors(v)
			if err != nil {
				return ConfigError(variable + " must list host, process, container, k8s or none")
			}
			c.ResourceDetectors = d
		}
	}
	if c.ServiceName == "" {
		if _, explicit := c.ResourceAttributes["service.name"]; !explicit && envAttrs["service.name"] != "" {
			c.ServiceName = envAttrs["service.name"]
		}
	}
	if len(envAttrs) > 0 {
		merged := make(map[string]any, len(envAttrs)+len(c.ResourceAttributes))
		for k, v := range envAttrs {
			merged[k] = v
		}
		for k, v := range c.ResourceAttributes {
			merged[k] = v
		}
		c.ResourceAttributes = merged
	}
	return nil
}

// applySignalEnv fills the Disable* switches.
func (c *Config) applySignalEnv(e envResolver) error {
	sdkDisabled, _, err := e.r.Bool("", "OTEL_SDK_DISABLED")
	if err != nil {
		return err
	}
	for _, s := range []struct {
		dst  *bool
		name string
	}{{&c.DisableTraces, "TRACES"}, {&c.DisableLogs, "LOGS"}, {&c.DisableMetrics, "METRICS"}} {
		if *s.dst {
			continue
		}
		if sdkDisabled {
			*s.dst = true
			continue
		}
		on, ok, err := e.boolean(s.name)
		if err != nil {
			return err
		}
		if ok {
			*s.dst = !on
			continue
		}
		if v, ok := e.r.String("", "OTEL_"+s.name+"_EXPORTER"); ok {
			*s.dst = strings.EqualFold(strings.TrimSpace(v), "none")
		}
	}
	return nil
}

// applyTracingEnv fills Sampler, SamplerRatio and Propagators.
func (c *Config) applyTracingEnv(e envResolver) error {
	if c.Sampler == "" {
		if v, variable, ok := e.find("SAMPLER", "OTEL_TRACES_SAMPLER"); ok {
			s := Sampler(strings.ToLower(strings.TrimSpace(v)))
			if !validSampler(s) {
				return ConfigError(variable + " names an unknown sampler")
			}
			c.Sampler = s
		}
	}
	if c.SamplerRatio == 0 {
		if v, variable, ok := e.find("SAMPLER_RATIO", "OTEL_TRACES_SAMPLER_ARG"); ok {
			ratio, err := strconv.ParseFloat(strings.TrimSpace(v), 64)
			if err != nil || ratio < 0 || ratio > 1 {
				return ConfigError(variable + " must be a number between 0 and 1")
			}
			c.SamplerRatio = ratio
		}
	}
	if len(c.Propagators) == 0 {
		if v, ok := e.str("PROPAGATORS", "OTEL_PROPAGATORS"); ok {
			for _, p := range strings.Split(v, ",") {
				if p = strings.ToLower(strings.TrimSpace(p)); p != "" && p != "none" {
					c.Propagators = append(c.Propagators, p)
				}
			}
		}
	}
	if !c.RuntimeMetrics {
		v, ok, err := e.boolean("RUNTIME_METRICS")
		if err != nil {
			return err
		}
		c.RuntimeMetrics = ok && v
	}
	return nil
}

// applyBatchEnv fills the batch settings from OTEL_BSP_*, OTEL_BLRP_* and
// OTEL_METRIC_EXPORT_INTERVAL.
func (c *Config) applyBatchEnv(e envResolver) error {
	if err := envBatch(e.r, "OTEL_BSP_", &c.TraceBatch); err != nil {
		return err
	}
	logBatch := OTelBatchOptions{
		ScheduledDelay: c.LogBatch.FlushInterval, ExportTimeout: c.LogBatch.ExportTimeout,
		MaxQueueSize: c.LogBatch.MaxQueueSize, MaxExportBatchSize: c.LogBatch.MaxBatchSize,
	}
	if err := envBatch(e.r, "OTEL_BLRP_", &logBatch); err != nil {
		return err
	}
	c.LogBatch.FlushInterval, c.LogBatch.ExportTimeout = logBatch.ScheduledDelay, logBatch.ExportTimeout
	c.LogBatch.MaxQueueSize, c.LogBatch.MaxBatchSize = logBatch.MaxQueueSize, logBatch.MaxExportBatchSize
	if c.MetricExportInterval == 0 {
		d, ok, err := e.duration("METRIC_EXPORT_INTERVAL", "OTEL_METRIC_EXPORT_INTERVAL")
		if err != nil {
			return err
		}
		if ok {
			c.MetricExportInterval = d
		}
	}
	return nil
}

func envBatch(r *EnvReader, prefix string, b *OTelBatchOptions) error {
	durations := []struct {
		dst  *time.Duration
		name string
	}{{&b.ScheduledDelay, "SCHEDULE_DELAY"}, {&b.ExportTimeout, "EXPORT_TIMEOUT"}}
	for _, d := range durations {
		if *d.dst != 0 {
			continue
		}
		v, ok, err := r.Duration("", prefix+d.name)
		if err != nil {
			return err
		}
		if ok {
			*d.dst = v
		}
	}
	ints := []struct {
		dst  *int
		name string
	}{{&b.MaxQueueSize, "MAX_QUEUE_SIZE"}, {&b.MaxExportBatchSize, "MAX_EXPORT_BATCH_SIZE"}}
	for _, n := range ints {
		if *n.dst != 0 {
			continue
		}
		v, ok, err := r.Int("", prefix+n.name)
		if err != nil {
			return err
		}
		if ok {
			*n.dst = v
		}
	}
	return nil
}

func parseProtocol(v string) (Protocol, error) {
	switch Protocol(strings.ToLower(strings.TrimSpace(v))) {
	case ProtocolGRPC:
		return ProtocolGRPC, nil
	case ProtocolHTTPProtobuf:
		return ProtocolHTTPProtobuf, nil
	}
	return "", errInvalidList
}

// Resource detector names.
const (
	DetectorHost      = "host"
	DetectorProcess   = "process"
	DetectorContainer = "container"
	DetectorK8s       = "k8s"
)

// AllResourceDetectors lists every resource detector (the default).
var AllResourceDetectors = []string{DetectorHost, DetectorProcess, DetectorContainer, DetectorK8s}

func parseDetectors(v string) ([]string, error) {
	out := []string{}
	for _, p := range strings.Split(v, ",") {
		p = strings.ToLower(strings.TrimSpace(p))
		switch p {
		case "":
		case "none":
			return []string{}, nil
		case DetectorHost, DetectorProcess, DetectorContainer, DetectorK8s:
			out = append(out, p)
		default:
			return nil, errInvalidList
		}
	}
	return out, nil
}

func validSampler(s Sampler) bool {
	switch s {
	case SamplerAlwaysOn, SamplerAlwaysOff, SamplerTraceIDRatio, SamplerParentBasedAlwaysOn,
		SamplerParentBasedAlwaysOff, SamplerParentBasedTraceIDRatio:
		return true
	}
	return false
}

// parseKeyValueList parses "k=v,k2=v2" (OpenTelemetry list format; keys and values are
// URL-decoded and trimmed).
func parseKeyValueList(s string) (map[string]string, error) {
	out := map[string]string{}
	for _, part := range strings.Split(s, ",") {
		if strings.TrimSpace(part) == "" {
			continue
		}
		k, v, ok := strings.Cut(part, "=")
		if !ok {
			return nil, errInvalidList
		}
		key, err1 := url.PathUnescape(strings.TrimSpace(k))
		val, err2 := url.PathUnescape(strings.TrimSpace(v))
		if err1 != nil || err2 != nil || key == "" {
			return nil, errInvalidList
		}
		out[key] = val
	}
	return out, nil
}

var errInvalidList = ConfigError("invalid key=value list")

// defaultServiceName is the OpenTelemetry fallback "unknown_service:<process name>".
func defaultServiceName() string {
	exe, err := os.Executable()
	if err != nil || exe == "" {
		return "unknown_service:go"
	}
	return "unknown_service:" + filepath.Base(exe)
}

// derivedURL returns base's scheme and hostname with port and path.
func derivedURL(base, port, path string) string {
	u, err := url.Parse(base)
	if err != nil || u.Host == "" {
		return ""
	}
	return u.Scheme + "://" + net.JoinHostPort(u.Hostname(), port) + path
}

// WithDefaults returns a copy with every unset field defaulted.
func (c Config) WithDefaults() Config {
	c.CommonConfig = c.withDefaults(DefaultURL)
	if c.Protocol == "" {
		c.Protocol = DefaultProtocol
	}
	if c.GRPCURL == "" {
		c.GRPCURL = DefaultGRPCURL
	}
	if c.Compression == "" {
		c.Compression = CompressionGzip
	}
	if c.ExportTimeout == 0 {
		c.ExportTimeout = DefaultOTLPExportTimeout
	}
	if c.HealthURL == "" {
		c.HealthURL = derivedURL(c.URL, DefaultHealthPort, "/")
	}
	if c.ZPagesURL == "" {
		c.ZPagesURL = derivedURL(c.URL, DefaultZPagesPort, "")
	}
	if c.ServiceName == "" {
		c.ServiceName = defaultServiceName()
	}
	if c.Sampler == "" {
		c.Sampler = SamplerParentBasedAlwaysOn
	}
	if c.SamplerRatio == 0 {
		c.SamplerRatio = 1
	}
	if len(c.Propagators) == 0 {
		c.Propagators = []string{PropagatorTraceContext, PropagatorBaggage}
	}
	if c.TraceBatch.ExportTimeout == 0 {
		c.TraceBatch.ExportTimeout = c.ExportTimeout
	}
	c.TraceBatch = TracingConfig{Batch: c.TraceBatch}.WithDefaults().Batch
	if c.MetricExportInterval == 0 {
		c.MetricExportInterval = DefaultMetricExportInterval
	}
	return c
}

// Validate checks the configuration and returns a CONFIG error.
func (c Config) Validate() error {
	d := c.WithDefaults()
	if err := d.validate(); err != nil {
		return err
	}
	switch d.Protocol {
	case ProtocolGRPC, ProtocolHTTPProtobuf:
	case ProtocolNative:
		return ConfigError("protocol native is not supported by the OpenTelemetry Collector; use grpc or http/protobuf")
	default:
		return ConfigError(fmt.Sprintf("unknown protocol %q (use grpc or http/protobuf)", string(d.Protocol)))
	}
	for _, u := range []struct{ name, value string }{
		{"grpcUrl", d.GRPCURL}, {"tracesUrl", d.TracesURL}, {"logsUrl", d.LogsURL}, {"metricsUrl", d.MetricsURL},
		{"healthUrl", d.HealthURL}, {"zpagesUrl", d.ZPagesURL}, {"telemetryUrl", d.TelemetryURL},
	} {
		if u.value != "" && !isHTTPURL(u.value) {
			return ConfigError(u.name + " must be an absolute http or https URL")
		}
	}
	for _, det := range d.ResourceDetectors {
		switch det {
		case DetectorHost, DetectorProcess, DetectorContainer, DetectorK8s:
		default:
			return ConfigError(fmt.Sprintf("unknown resource detector %q (use host, process, container or k8s)", det))
		}
	}
	if d.ExportTimeout < 0 || d.MetricExportInterval < 0 {
		return ConfigError("exportTimeout and metricExportInterval must not be negative")
	}
	if err := d.LogBatch.Validate(); err != nil {
		return err
	}
	// The span settings share the template rules (sampler, ratio, propagators,
	// compression, batch, TLS).
	return d.TracingConfig().Validate()
}

func isHTTPURL(s string) bool {
	u, err := url.Parse(s)
	return err == nil && (u.Scheme == "http" || u.Scheme == "https") && u.Host != ""
}

// SignalEndpoint returns the export endpoint of a signal: the per-signal URL when
// set, else for HTTP URL + "/v1/<signal>" and for gRPC GRPCURL.
func (c Config) SignalEndpoint(s Signal) string {
	d := c.WithDefaults()
	var explicit string
	switch s {
	case SignalTraces:
		explicit = d.TracesURL
	case SignalLogs:
		explicit = d.LogsURL
	case SignalMetrics:
		explicit = d.MetricsURL
	}
	if explicit != "" {
		return explicit
	}
	if d.Protocol == ProtocolGRPC {
		return d.GRPCURL
	}
	return strings.TrimSuffix(d.URL, "/") + "/v1/" + string(s)
}

// SignalHeaders returns Headers merged with the per-signal headers (signal wins).
func (c Config) SignalHeaders(s Signal) map[string]string {
	var extra map[string]string
	switch s {
	case SignalTraces:
		extra = c.TracesHeaders
	case SignalLogs:
		extra = c.LogsHeaders
	case SignalMetrics:
		extra = c.MetricsHeaders
	}
	out := make(map[string]string, len(c.Headers)+len(extra))
	for k, v := range c.Headers {
		out[k] = v
	}
	for k, v := range extra {
		out[k] = v
	}
	return out
}

// TracingConfig returns the TracingConfig of the traces signal (endpoint, protocol,
// headers, TLS, compression, resource, sampler, propagators, batch, setGlobal).
func (c Config) TracingConfig() TracingConfig {
	d := c.WithDefaults()
	batch := d.TraceBatch
	return TracingConfig{
		ServiceName:        d.ServiceName,
		ServiceVersion:     d.ServiceVersion,
		Environment:        d.Environment,
		ResourceAttributes: d.ResourceAttributes,
		Endpoint:           d.SignalEndpoint(SignalTraces),
		Protocol:           d.Protocol,
		Headers:            d.SignalHeaders(SignalTraces),
		Sampler:            d.Sampler,
		SamplerRatio:       d.SamplerRatio,
		Propagators:        d.Propagators,
		Batch:              batch,
		TLS:                d.TLS,
		Compression:        d.Compression,
		DisableSetGlobal:   d.DisableSetGlobal,
	}
}

// secretValues returns every sensitive header value of the config (for redaction).
func (c Config) secretValues() []string {
	var out []string
	for _, h := range []map[string]string{c.Headers, c.TracesHeaders, c.LogsHeaders, c.MetricsHeaders} {
		for k, v := range h {
			if IsSensitiveHeader(k) && v != "" {
				out = append(out, v)
			}
		}
	}
	return out
}

func describeHeaders(h map[string]string) string {
	r := RedactHeaderMap(h)
	keys := make([]string, 0, len(r))
	for k := range r {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	parts := make([]string, len(keys))
	for i, k := range keys {
		parts[i] = k + "=" + r[k]
	}
	return "{" + strings.Join(parts, " ") + "}"
}

// String renders the configuration with every credential redacted.
func (c Config) String() string {
	return fmt.Sprintf("Config{%s GRPCURL:%s Protocol:%s TracesURL:%s LogsURL:%s MetricsURL:%s TracesHeaders:%s LogsHeaders:%s MetricsHeaders:%s Compression:%s ExportTimeout:%s HealthURL:%s ZPagesURL:%s TelemetryURL:%s ServiceName:%q Traces:%t Logs:%t Metrics:%t RuntimeMetrics:%t}",
		c.describe(), RedactURL(c.GRPCURL), c.Protocol, RedactURL(c.TracesURL), RedactURL(c.LogsURL), RedactURL(c.MetricsURL),
		describeHeaders(c.TracesHeaders), describeHeaders(c.LogsHeaders), describeHeaders(c.MetricsHeaders),
		c.Compression, c.ExportTimeout, RedactURL(c.HealthURL), RedactURL(c.ZPagesURL), RedactURL(c.TelemetryURL),
		c.ServiceName, !c.DisableTraces, !c.DisableLogs, !c.DisableMetrics, c.RuntimeMetrics)
}

// GoString renders the configuration with every credential redacted.
func (c Config) GoString() string { return c.String() }
