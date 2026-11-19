package otelcollector

import (
	"errors"
	"fmt"
	"strings"
	"testing"
	"time"
)

func mustConfig(t *testing.T, explicit Config, env map[string]string) Config {
	t.Helper()
	c, err := ConfigFromEnvWith(explicit, env)
	if err != nil {
		t.Fatalf("ConfigFromEnvWith: %v", err)
	}
	return c
}

func TestConfigDefaults(t *testing.T) {
	c := mustConfig(t, Config{}, map[string]string{})
	if c.URL != DefaultURL || c.GRPCURL != DefaultGRPCURL || c.Protocol != ProtocolHTTPProtobuf ||
		c.Compression != CompressionGzip || c.ExportTimeout != 10*time.Second ||
		c.HealthURL != "http://127.0.0.1:13133/" || c.ZPagesURL != "http://127.0.0.1:55679" ||
		c.TelemetryURL != "" || !strings.HasPrefix(c.ServiceName, "unknown_service:") ||
		c.Sampler != SamplerParentBasedAlwaysOn || c.SamplerRatio != 1 || len(c.Propagators) != 2 ||
		c.TraceBatch.ExportTimeout != 10*time.Second || c.TraceBatch.ScheduledDelay != DefaultTraceScheduledDelay ||
		c.MetricExportInterval != time.Minute || c.DisableTraces || c.DisableLogs || c.DisableMetrics || c.RuntimeMetrics {
		t.Fatalf("defaults: %s", c)
	}
	if got := c.SignalEndpoint(SignalLogs); got != "http://127.0.0.1:4318/v1/logs" {
		t.Fatalf("logs endpoint %q", got)
	}
	g := Config{Protocol: ProtocolGRPC, GRPCURL: "http://c:4317", MetricsURL: "http://m:4317"}
	if g.SignalEndpoint(SignalTraces) != "http://c:4317" || g.SignalEndpoint(SignalMetrics) != "http://m:4317" {
		t.Fatal("grpc endpoints")
	}
	h := Config{CommonConfig: CommonConfig{URL: "https://gw.example:443/base/"}}
	if h.SignalEndpoint(SignalTraces) != "https://gw.example:443/base/v1/traces" {
		t.Fatalf("base path: %s", h.SignalEndpoint(SignalTraces))
	}
	if d := (Config{CommonConfig: CommonConfig{URL: "https://col.internal:4318"}}).WithDefaults(); d.HealthURL != "https://col.internal:13133/" || d.ZPagesURL != "https://col.internal:55679" || d.GRPCURL != "https://col.internal:4317" {
		t.Fatalf("derived: %s %s %s", d.HealthURL, d.ZPagesURL, d.GRPCURL)
	}
	if d := (Config{CommonConfig: CommonConfig{URL: "otel:4318"}}).WithDefaults(); d.GRPCURL != DefaultGRPCURL {
		t.Fatalf("non-http URL keeps the default gRPC target: %s", d.GRPCURL)
	}
	if derivedURL("::bad", "1", "/") != "" {
		t.Fatal("derivedURL invalid")
	}
}

func TestConfigEnv(t *testing.T) {
	t.Run("core.config.env-precedence", func(t *testing.T) {
		env := map[string]string{
			"OTEL_EXPORTER_OTLP_ENDPOINT": "http://otel:4318",
			aliasPrefix + "URL":           "http://alias:4318",
		}
		if c := mustConfig(t, Config{}, env); c.URL != "http://alias:4318" {
			t.Fatalf("alias beats OTEL: %s", c.URL)
		}
		env[EnvPrefix+"URL"] = "http://grow:4318"
		if c := mustConfig(t, Config{}, env); c.URL != "http://grow:4318" {
			t.Fatalf("GROW beats alias: %s", c.URL)
		}
		if c := mustConfig(t, Config{CommonConfig: CommonConfig{URL: "http://explicit:1"}}, env); c.URL != "http://explicit:1" {
			t.Fatalf("explicit wins: %s", c.URL)
		}
		if c := mustConfig(t, Config{}, map[string]string{"OTEL_EXPORTER_OTLP_ENDPOINT": "http://otel:4318"}); c.URL != "http://otel:4318" || c.HealthURL != "http://otel:13133/" {
			t.Fatalf("OTEL endpoint: %s", c)
		}
		_, err := ConfigFromEnvWith(Config{}, map[string]string{EnvPrefix + "TOTAL_TIMEOUT": "forever-xyz"})
		if !errors.Is(err, ErrConfig) || !strings.Contains(err.Error(), EnvPrefix+"TOTAL_TIMEOUT") || strings.Contains(err.Error(), "forever-xyz") {
			t.Fatalf("invalid value: %v", err)
		}
		t.Setenv(EnvPrefix+"URL", "http://process-env:9")
		if c, err := ConfigFromEnv(); err != nil || c.URL != "http://process-env:9" {
			t.Fatalf("process env: %v %s", err, c.URL)
		}
	})

	t.Run("grpc endpoint follows the protocol", func(t *testing.T) {
		env := map[string]string{"OTEL_EXPORTER_OTLP_PROTOCOL": "grpc", "OTEL_EXPORTER_OTLP_ENDPOINT": "http://otel:4317"}
		c := mustConfig(t, Config{}, env)
		if c.Protocol != ProtocolGRPC || c.GRPCURL != "http://otel:4317" || c.URL != DefaultURL {
			t.Fatalf("grpc: %s", c)
		}
		env[EnvPrefix+"GRPC_URL"] = "http://grow:4317"
		env[EnvPrefix+"PROTOCOL"] = "http/protobuf"
		c = mustConfig(t, Config{}, env)
		if c.Protocol != ProtocolHTTPProtobuf || c.GRPCURL != "http://grow:4317" || c.URL != "http://otel:4317" {
			t.Fatalf("grow protocol: %s", c)
		}
	})

	t.Run("all variables", func(t *testing.T) {
		env := map[string]string{
			EnvPrefix + "TRACES_URL":                "http://t:1/x",
			"OTEL_EXPORTER_OTLP_LOGS_ENDPOINT":      "http://l:1/y",
			EnvPrefix + "METRICS_URL":               "http://m:1/z",
			"OTEL_EXPORTER_OTLP_HEADERS":            "X-Generic=g, Authorization=Bearer%20otel-SECRET",
			"OTEL_EXPORTER_OTLP_TRACES_HEADERS":     "X-Traces=t",
			"OTEL_EXPORTER_OTLP_LOGS_HEADERS":       "X-Logs=l",
			"OTEL_EXPORTER_OTLP_METRICS_HEADERS":    "X-Generic=metrics",
			"OTEL_EXPORTER_OTLP_COMPRESSION":        "none",
			"OTEL_EXPORTER_OTLP_TIMEOUT":            "2500",
			"OTEL_EXPORTER_OTLP_CERTIFICATE":        "/ca.pem",
			"OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE": "/c.pem",
			"OTEL_EXPORTER_OTLP_CLIENT_KEY":         "/k.pem",
			EnvPrefix + "HEALTH_URL":                "http://h:13133/",
			EnvPrefix + "ZPAGES_URL":                "http://z:55679",
			EnvPrefix + "TELEMETRY_URL":             "http://z:8888/metrics",
			"OTEL_RESOURCE_ATTRIBUTES":              "service.name=from-attrs,team=core,deployment.environment.name=prod",
			EnvPrefix + "SERVICE_VERSION":           "1.2.3",
			EnvPrefix + "ENVIRONMENT":               "staging",
			EnvPrefix + "RESOURCE_DETECTORS":        "host, k8s",
			"OTEL_TRACES_EXPORTER":                  "none",
			EnvPrefix + "METRICS":                   "false",
			"OTEL_LOGS_EXPORTER":                    "otlp",
			"OTEL_TRACES_SAMPLER":                   "parentbased_traceidratio",
			"OTEL_TRACES_SAMPLER_ARG":               "0.25",
			"OTEL_PROPAGATORS":                      "tracecontext, b3, none",
			"OTEL_BSP_SCHEDULE_DELAY":               "200",
			"OTEL_BSP_MAX_QUEUE_SIZE":               "100",
			"OTEL_BSP_MAX_EXPORT_BATCH_SIZE":        "10",
			"OTEL_BSP_EXPORT_TIMEOUT":               "3000",
			"OTEL_BLRP_SCHEDULE_DELAY":              "300",
			"OTEL_BLRP_EXPORT_TIMEOUT":              "4000",
			"OTEL_BLRP_MAX_QUEUE_SIZE":              "50",
			"OTEL_BLRP_MAX_EXPORT_BATCH_SIZE":       "5",
			"OTEL_METRIC_EXPORT_INTERVAL":           "1500",
			EnvPrefix + "RUNTIME_METRICS":           "yes",
		}
		c := mustConfig(t, Config{}, env)
		if c.TracesURL != "http://t:1/x" || c.LogsURL != "http://l:1/y" || c.MetricsURL != "http://m:1/z" ||
			c.SignalEndpoint(SignalTraces) != "http://t:1/x" {
			t.Fatalf("urls: %s", c)
		}
		th, mh := c.SignalHeaders(SignalTraces), c.SignalHeaders(SignalMetrics)
		if th["X-Generic"] != "g" || th["X-Traces"] != "t" || th["Authorization"] != "Bearer otel-SECRET" || mh["X-Generic"] != "metrics" ||
			c.SignalHeaders(SignalLogs)["X-Logs"] != "l" || len(c.SignalHeaders("other")) != 2 {
			t.Fatalf("headers: %v %v", th, mh)
		}
		if c.Compression != CompressionNone || c.ExportTimeout != 2500*time.Millisecond ||
			c.TLS.CACertPath != "/ca.pem" || c.TLS.ClientCertPath != "/c.pem" || c.TLS.ClientKeyPath != "/k.pem" {
			t.Fatalf("transport: %s", c)
		}
		if c.HealthURL != "http://h:13133/" || c.ZPagesURL != "http://z:55679" || c.TelemetryURL != "http://z:8888/metrics" {
			t.Fatalf("admin urls: %s", c)
		}
		if c.ServiceName != "from-attrs" || c.ServiceVersion != "1.2.3" || c.Environment != "staging" ||
			c.ResourceAttributes["team"] != "core" || len(c.ResourceDetectors) != 2 {
			t.Fatalf("resource: %s %v", c, c.ResourceAttributes)
		}
		if !c.DisableTraces || !c.DisableMetrics || c.DisableLogs || !c.RuntimeMetrics {
			t.Fatalf("signals: %s", c)
		}
		if c.Sampler != SamplerParentBasedTraceIDRatio || c.SamplerRatio != 0.25 || fmt.Sprint(c.Propagators) != "[tracecontext b3]" {
			t.Fatalf("sampling: %s %v", c.Sampler, c.Propagators)
		}
		if c.TraceBatch != (OTelBatchOptions{MaxQueueSize: 100, MaxExportBatchSize: 10, ScheduledDelay: 200 * time.Millisecond, ExportTimeout: 3 * time.Second}) {
			t.Fatalf("trace batch: %+v", c.TraceBatch)
		}
		if c.LogBatch.FlushInterval != 300*time.Millisecond || c.LogBatch.ExportTimeout != 4*time.Second ||
			c.LogBatch.MaxQueueSize != 50 || c.LogBatch.MaxBatchSize != 5 || c.MetricExportInterval != 1500*time.Millisecond {
			t.Fatalf("log batch: %+v %s", c.LogBatch, c.MetricExportInterval)
		}
		forms := fmt.Sprintf("%v %+v %#v", c, c, c)
		if strings.Contains(forms, "otel-SECRET") {
			t.Fatalf("core.redaction.config: %s", forms)
		}
	})

	t.Run("grow values beat OTEL values", func(t *testing.T) {
		env := map[string]string{
			EnvPrefix + "HEADERS":                "X-Grow=1",
			"OTEL_EXPORTER_OTLP_HEADERS":         "X-Otel=1",
			EnvPrefix + "COMPRESSION":            "GZIP",
			EnvPrefix + "EXPORT_TIMEOUT":         "3s",
			"OTEL_EXPORTER_OTLP_TIMEOUT":         "100",
			EnvPrefix + "SERVICE_NAME":           "grow-svc",
			"OTEL_SERVICE_NAME":                  "otel-svc",
			EnvPrefix + "RESOURCE_ATTRIBUTES":    "a=grow",
			"OTEL_RESOURCE_ATTRIBUTES":           "a=otel,b=otel",
			EnvPrefix + "SAMPLER":                "always_off",
			EnvPrefix + "SAMPLER_RATIO":          "0.5",
			EnvPrefix + "PROPAGATORS":            "b3multi",
			EnvPrefix + "TRACES":                 "true",
			"OTEL_TRACES_EXPORTER":               "none",
			EnvPrefix + "METRIC_EXPORT_INTERVAL": "2s",
			EnvPrefix + "RESOURCE_DETECTORS":     "none",
		}
		c := mustConfig(t, Config{ResourceAttributes: map[string]any{"c": "explicit", "a": "explicit"}}, env)
		if c.Headers["X-Grow"] != "1" || c.Headers["X-Otel"] != "" || c.Compression != CompressionGzip ||
			c.ExportTimeout != 3*time.Second || c.ServiceName != "grow-svc" || c.ResourceAttributes["a"] != "explicit" ||
			c.ResourceAttributes["b"] != nil || c.ResourceAttributes["c"] != "explicit" || c.Sampler != SamplerAlwaysOff ||
			c.SamplerRatio != 0.5 || c.Propagators[0] != PropagatorB3Multi || c.DisableTraces ||
			c.MetricExportInterval != 2*time.Second || c.ResourceDetectors == nil || len(c.ResourceDetectors) != 0 {
			t.Fatalf("grow: %s %v", c, c.ResourceAttributes)
		}
		c = mustConfig(t, Config{}, map[string]string{"OTEL_SERVICE_NAME": "otel-svc", "OTEL_RESOURCE_ATTRIBUTES": "service.name=attr"})
		if c.ServiceName != "otel-svc" {
			t.Fatalf("OTEL_SERVICE_NAME beats service.name: %s", c.ServiceName)
		}
		c = mustConfig(t, Config{ResourceAttributes: map[string]any{"service.name": "explicit"}}, map[string]string{"OTEL_RESOURCE_ATTRIBUTES": "service.name=attr"})
		if c.ResourceAttributes["service.name"] != "explicit" || c.ServiceName == "attr" {
			t.Fatalf("explicit attr: %s", c.ServiceName)
		}
		c = mustConfig(t, Config{}, map[string]string{"OTEL_SDK_DISABLED": "true", EnvPrefix + "TRACES": "true"})
		if !c.DisableTraces || !c.DisableLogs || !c.DisableMetrics {
			t.Fatalf("sdk disabled: %s", c)
		}
	})

	t.Run("invalid values name the variable", func(t *testing.T) {
		for name, value := range map[string]string{
			"OTEL_EXPORTER_OTLP_PROTOCOL":       "http/json",
			EnvPrefix + "HEADERS":               "novalue",
			"OTEL_EXPORTER_OTLP_HEADERS":        "a=%zz",
			"OTEL_EXPORTER_OTLP_TRACES_HEADERS": "=x",
			"OTEL_EXPORTER_OTLP_COMPRESSION":    "brotli",
			"OTEL_EXPORTER_OTLP_TIMEOUT":        "soon",
			"OTEL_RESOURCE_ATTRIBUTES":          "broken",
			EnvPrefix + "RESOURCE_DETECTORS":    "gcp",
			"OTEL_SDK_DISABLED":                 "maybe",
			EnvPrefix + "LOGS":                  "perhaps",
			"OTEL_TRACES_SAMPLER":               "jaeger_remote",
			"OTEL_TRACES_SAMPLER_ARG":           "2",
			EnvPrefix + "RUNTIME_METRICS":       "sure",
			"OTEL_BSP_SCHEDULE_DELAY":           "later",
			"OTEL_BLRP_MAX_QUEUE_SIZE":          "lots",
			"OTEL_METRIC_EXPORT_INTERVAL":       "often",
			EnvPrefix + "INSECURE_SKIP_VERIFY":  "kinda",
		} {
			_, err := ConfigFromEnvWith(Config{}, map[string]string{name: value})
			if !errors.Is(err, ErrConfig) || !strings.Contains(err.Error(), name) || strings.Contains(err.Error(), value) && len(value) > 3 {
				t.Fatalf("%s=%s: %v", name, value, err)
			}
		}
		if _, err := ConfigFromEnvWith(Config{}, map[string]string{EnvPrefix + "URL": "ftp://nope"}); !errors.Is(err, ErrConfig) {
			t.Fatalf("invalid url: %v", err)
		}
	})
}

func TestConfigValidate(t *testing.T) {
	bad := []Config{
		{Protocol: ProtocolNative},
		{Protocol: "http/json"},
		{GRPCURL: "localhost:4317"},
		{TracesURL: "ftp://x"},
		{HealthURL: "nope"},
		{ExportTimeout: -1},
		{MetricExportInterval: -1},
		{LogBatch: BatchOptions{MaxQueueSize: -1}},
		{Sampler: "sometimes"},
		{SamplerRatio: 2},
		{Propagators: []string{"carrier-pigeon"}},
		{Compression: "zstd"},
		{ResourceDetectors: []string{"gcp"}},
		{TraceBatch: OTelBatchOptions{MaxQueueSize: -1}},
	}
	for _, c := range bad {
		if err := c.Validate(); !errors.Is(err, ErrConfig) {
			t.Fatalf("%s: %v", c, err)
		}
	}
	if err := (Config{Protocol: ProtocolNative}).Validate(); !strings.Contains(err.Error(), "native") {
		t.Fatalf("native message: %v", err)
	}
	if err := (Config{ResourceDetectors: AllResourceDetectors}).Validate(); err != nil {
		t.Fatal(err)
	}
}

func TestConfigTracingConfig(t *testing.T) {
	c := Config{
		CommonConfig:       CommonConfig{URL: "http://c:4318", Headers: map[string]string{"Authorization": "Bearer x-SECRET"}},
		ServiceName:        "svc",
		ServiceVersion:     "2.0.0",
		Environment:        "dev",
		ResourceAttributes: map[string]any{"k": "v"},
		TracesHeaders:      map[string]string{"X-T": "1"},
		Sampler:            SamplerAlwaysOn,
		DisableSetGlobal:   true,
	}
	tc := c.TracingConfig()
	if tc.Endpoint != "http://c:4318/v1/traces" || tc.Protocol != ProtocolHTTPProtobuf || tc.ServiceName != "svc" ||
		tc.ServiceVersion != "2.0.0" || tc.Environment != "dev" || tc.Headers["X-T"] != "1" || tc.Headers["Authorization"] == "" ||
		tc.Sampler != SamplerAlwaysOn || !tc.DisableSetGlobal || tc.ResourceAttributes["k"] != "v" {
		t.Fatalf("tracing config: %s", tc)
	}
	if s := c.String(); strings.Contains(s, "x-SECRET") || !strings.Contains(s, "ServiceName:\"svc\"") {
		t.Fatalf("String: %s", s)
	}
	if c.GoString() != c.String() {
		t.Fatal("GoString")
	}
	if len(c.secretValues()) != 1 {
		t.Fatalf("secrets: %v", c.secretValues())
	}
	if _, err := parseProtocol("native"); err == nil {
		t.Fatal("parseProtocol native")
	}
	if d, err := parseDetectors(" ,host"); err != nil || len(d) != 1 {
		t.Fatalf("detectors: %v %v", d, err)
	}
}
