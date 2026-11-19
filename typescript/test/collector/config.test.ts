import { inspect } from "node:util";
import { describe, expect, it } from "vitest";
import {
  ConfigError,
  OtelCollectorConfig,
  PACKAGE_KEY,
  REDACTED,
  RetryPolicy,
  parseKeyValueList,
} from "../../src/index.js";

describe("OtelCollectorConfig defaults", () => {
  it("uses the documented defaults and freezes", () => {
    const c = new OtelCollectorConfig();
    expect(c.url).toBe("http://127.0.0.1:4318");
    expect(c.grpcUrl).toBe("http://127.0.0.1:4317");
    expect(c.protocol).toBe("http/protobuf");
    expect(c.compression).toBe("gzip");
    expect(c.exportTimeoutMs).toBe(10_000);
    expect(c.requestTimeoutMs).toBe(30_000);
    expect(c.healthUrl).toBe("http://127.0.0.1:13133/");
    expect(c.healthUrlExplicit).toBe(false);
    expect(c.zpagesUrl).toBe("http://127.0.0.1:55679");
    expect(c.telemetryUrl).toBeUndefined();
    expect(c.serviceName).toMatch(/^unknown_service:/);
    expect(c.traces && c.logs && c.metrics).toBe(true);
    expect(c.sampler).toBe("parentbased_always_on");
    expect(c.samplerRatio).toBe(1);
    expect(c.propagators).toEqual(["tracecontext", "baggage"]);
    expect(c.metricExportIntervalMs).toBe(60_000);
    expect(c.runtimeMetrics).toBe(false);
    expect(c.resourceDetectors).toEqual(["host", "process", "container", "k8s"]);
    expect(c.retry).toBeInstanceOf(RetryPolicy);
    expect(Object.isFrozen(c)).toBe(true);
    expect(OtelCollectorConfig.from(c)).toBe(c);
    expect(OtelCollectorConfig.from().url).toBe(c.url);
    expect(OtelCollectorConfig.ENV_PREFIX).toBe(
      `GROW_${PACKAGE_KEY.toUpperCase().replaceAll("-", "_")}_`,
    );
  });

  it("derives signal URLs, endpoints and headers", () => {
    const c = new OtelCollectorConfig({
      url: "https://otel.example.com/base/",
      headers: { authorization: "Bearer a", "x-team": "t" },
      logsHeaders: { "x-team": "logs" },
      metricsUrl: "https://m.example.com/custom",
    });
    expect(c.signalUrl("traces")).toBe("https://otel.example.com/base/v1/traces");
    expect(c.signalUrl("metrics")).toBe("https://m.example.com/custom");
    expect(c.endpointFor("logs")).toBe("https://otel.example.com/base/v1/logs");
    expect(c.headersFor("logs")).toEqual({ authorization: "Bearer a", "x-team": "logs" });
    expect(c.headersFor("traces")).toEqual({ authorization: "Bearer a", "x-team": "t" });
    expect(c.healthUrl).toBe("https://otel.example.com:13133/");
    expect(c.grpcUrl).toBe("https://otel.example.com:4317");
    const g = new OtelCollectorConfig({ protocol: "grpc", grpcUrl: "http://c:4317" });
    expect(g.endpointFor("traces")).toBe("http://c:4317");
    const gs = new OtelCollectorConfig({
      protocol: "grpc",
      url: "http://otel:4318",
      tracesUrl: "http://traces-gw:4317",
    });
    expect(gs.endpointFor("traces")).toBe("http://traces-gw:4317");
    expect(gs.endpointFor("logs")).toBe("http://otel:4317");
    const v6 = new OtelCollectorConfig({ url: "http://[::1]:4318" });
    expect(v6.zpagesUrl).toBe("http://[::1]:55679");
  });

  it("keeps explicit settings", () => {
    const hooks = {};
    const logger = { debug() {}, info() {}, warn() {}, error() {} };
    const c = new OtelCollectorConfig({
      proxy: "http://p:1",
      userAgentSuffix: "s",
      hooks,
      logger,
      healthUrl: "http://h:1/",
      zpagesUrl: "http://z:2",
      telemetryUrl: "http://t:3/metrics",
      tracesUrl: "http://x/t",
      logsUrl: "http://x/l",
      serviceName: " svc ",
      serviceVersion: "1.0",
      environment: "dev",
      resourceAttributes: { team: "a" },
      traces: false,
      sampler: "traceidratio",
      samplerRatio: 0.5,
      propagators: ["b3"],
      tracesBatch: { scheduledDelayMs: 100 },
      logsBatch: { maxBatchSize: 5 },
      runtimeMetrics: true,
      resourceDetectors: ["host"],
      compression: "none",
    });
    expect(c.proxy).toBe("http://p:1");
    expect(c.userAgentSuffix).toBe("s");
    expect(c.hooks).toBe(hooks);
    expect(c.logger).toBe(logger);
    expect(c.healthUrlExplicit).toBe(true);
    expect(c.serviceName).toBe("svc");
    expect(c.serviceVersion).toBe("1.0");
    expect(c.environment).toBe("dev");
    expect(c.traces).toBe(false);
    expect(c.tracesBatch).toEqual({ scheduledDelayMs: 100 });
    expect(c.logsBatch).toEqual({ maxBatchSize: 5 });
    const json = c.toJSON();
    expect(json).toMatchObject({
      tracesUrl: "http://x/t",
      logsUrl: "http://x/l",
      telemetryUrl: "http://t:3/metrics",
      serviceVersion: "1.0",
      environment: "dev",
      compression: "none",
    });
  });

  it("validate() raises ConfigError naming the field", () => {
    const bad: [Record<string, unknown>, RegExp][] = [
      [{ url: "ftp://x" }, /url/],
      [{ grpcUrl: "nope" }, /grpcUrl/],
      [{ healthUrl: "x" }, /healthUrl/],
      [{ telemetryUrl: "x" }, /telemetryUrl/],
      [{ protocol: "native" }, /native protocol/],
      [{ protocol: "http/json" }, /grpc or http\/protobuf/],
      [{ compression: "br" }, /compression/],
      [{ exportTimeoutMs: 0 }, /exportTimeoutMs/],
      [{ metricExportIntervalMs: -1 }, /metricExportIntervalMs/],
      [{ sampler: "nope" }, /sampler/],
      [{ samplerRatio: 2 }, /samplerRatio/],
      [{ resourceDetectors: ["gcp"] }, /gcp/],
    ];
    for (const [opts, re] of bad) {
      expect(() => new OtelCollectorConfig(opts as never)).toThrow(ConfigError);
      expect(() => new OtelCollectorConfig(opts as never)).toThrow(re);
    }
  });

  it("core.redaction.config: toString/JSON/inspect hide header values", () => {
    const secret = "sup3r-secret-token";
    const c = new OtelCollectorConfig({
      url: "http://user:pw@h:4318",
      headers: { authorization: `Bearer ${secret}`, "x-plain": "visible" },
      tracesHeaders: { "x-api-key": secret },
      logsHeaders: { "x-token": "logs-secret" },
      metricsHeaders: { "x-other": "o" },
    });
    for (const text of [c.toString(), JSON.stringify(c), inspect(c)]) {
      expect(text).not.toContain(secret);
      expect(text).not.toContain("logs-secret");
      expect(text).not.toContain(":pw@");
      expect(text).toContain(REDACTED);
    }
    expect(c.knownSecrets()).toEqual([`Bearer ${secret}`, secret, "logs-secret"]);
  });
});

describe("OtelCollectorConfig.fromEnv", () => {
  it("reads GROW_OPENTELEMETRY_COLLECTOR_* settings", () => {
    const c = OtelCollectorConfig.fromEnv(
      {},
      {
        GROW_OPENTELEMETRY_COLLECTOR_URL: "http://c:4318",
        GROW_OPENTELEMETRY_COLLECTOR_GRPC_URL: "http://c:4317",
        GROW_OPENTELEMETRY_COLLECTOR_PROTOCOL: "GRPC",
        GROW_OPENTELEMETRY_COLLECTOR_TRACES_URL: "http://c/t",
        GROW_OPENTELEMETRY_COLLECTOR_LOGS_URL: "http://c/l",
        GROW_OPENTELEMETRY_COLLECTOR_METRICS_URL: "http://c/m",
        GROW_OPENTELEMETRY_COLLECTOR_HEADERS: "authorization=Bearer%20x,x-a=1",
        GROW_OPENTELEMETRY_COLLECTOR_COMPRESSION: "none",
        GROW_OPENTELEMETRY_COLLECTOR_EXPORT_TIMEOUT: "2s",
        GROW_OPENTELEMETRY_COLLECTOR_HEALTH_URL: "http://c:13133/",
        GROW_OPENTELEMETRY_COLLECTOR_ZPAGES_URL: "http://c:55679",
        GROW_OPENTELEMETRY_COLLECTOR_TELEMETRY_URL: "http://c:8888/metrics",
        GROW_OPENTELEMETRY_COLLECTOR_SERVICE_NAME: "svc",
        GROW_OPENTELEMETRY_COLLECTOR_SERVICE_VERSION: "2.0",
        GROW_OPENTELEMETRY_COLLECTOR_ENVIRONMENT: "prod",
        GROW_OPENTELEMETRY_COLLECTOR_RESOURCE_ATTRIBUTES: "team=core",
        GROW_OPENTELEMETRY_COLLECTOR_TRACES: "false",
        GROW_OPENTELEMETRY_COLLECTOR_LOGS: "yes",
        GROW_OPENTELEMETRY_COLLECTOR_METRICS: "0",
        GROW_OPENTELEMETRY_COLLECTOR_SAMPLER: "traceidratio",
        GROW_OPENTELEMETRY_COLLECTOR_SAMPLER_RATIO: "0.25",
        GROW_OPENTELEMETRY_COLLECTOR_PROPAGATORS: "tracecontext, b3",
        GROW_OPENTELEMETRY_COLLECTOR_METRIC_EXPORT_INTERVAL: "5000",
        GROW_OPENTELEMETRY_COLLECTOR_RUNTIME_METRICS: "on",
        GROW_OPENTELEMETRY_COLLECTOR_RESOURCE_DETECTORS: "host,k8s",
        GROW_OPENTELEMETRY_COLLECTOR_REQUEST_TIMEOUT: "1500",
      },
    );
    expect(c).toMatchObject({
      url: "http://c:4318",
      grpcUrl: "http://c:4317",
      protocol: "grpc",
      tracesUrl: "http://c/t",
      logsUrl: "http://c/l",
      metricsUrl: "http://c/m",
      headers: { authorization: "Bearer x", "x-a": "1" },
      compression: "none",
      exportTimeoutMs: 2000,
      healthUrl: "http://c:13133/",
      zpagesUrl: "http://c:55679",
      telemetryUrl: "http://c:8888/metrics",
      serviceName: "svc",
      serviceVersion: "2.0",
      environment: "prod",
      resourceAttributes: { team: "core" },
      traces: false,
      logs: true,
      metrics: false,
      sampler: "traceidratio",
      samplerRatio: 0.25,
      propagators: ["tracecontext", "b3"],
      metricExportIntervalMs: 5000,
      runtimeMetrics: true,
      resourceDetectors: ["host", "k8s"],
      requestTimeoutMs: 1500,
    });
  });

  it("core.config.env-precedence: explicit > GROW_* > GROW_OTEL_* > per-signal OTEL_* > OTEL_* > default", () => {
    const env = {
      GROW_OPENTELEMETRY_COLLECTOR_SERVICE_NAME: "primary",
      GROW_OTEL_SERVICE_NAME: "alias",
      GROW_OTEL_URL: "http://alias:4318",
      OTEL_SERVICE_NAME: "otel",
      OTEL_EXPORTER_OTLP_ENDPOINT: "http://otel:4318",
      OTEL_EXPORTER_OTLP_TRACES_ENDPOINT: "http://otel-traces/v1/traces",
      OTEL_EXPORTER_OTLP_HEADERS: "a=generic,b=generic",
      OTEL_EXPORTER_OTLP_TRACES_HEADERS: "a=traces",
      GROW_OTEL_HEADERS: "b=alias",
      GROW_OPENTELEMETRY_COLLECTOR_RESOURCE_ATTRIBUTES: "k1=grow",
      OTEL_RESOURCE_ATTRIBUTES: "k1=otel,k2=otel,service.name=from-attrs",
    };
    const c = OtelCollectorConfig.fromEnv({ resourceAttributes: { k3: "explicit" } }, env);
    expect(c.serviceName).toBe("primary");
    expect(c.url).toBe("http://alias:4318");
    expect(c.signalUrl("traces")).toBe("http://otel-traces/v1/traces");
    expect(c.headers).toEqual({ a: "generic", b: "alias" });
    expect(c.headersFor("traces")).toEqual({ a: "traces", b: "alias" });
    expect(c.resourceAttributes).toEqual({ k1: "grow", k2: "otel", k3: "explicit" });
    expect(OtelCollectorConfig.fromEnv({ serviceName: "explicit" }, env).serviceName).toBe(
      "explicit",
    );
    expect(OtelCollectorConfig.fromEnv({}, { GROW_OTEL_SERVICE_NAME: "alias" }).serviceName).toBe(
      "alias",
    );
    expect(
      OtelCollectorConfig.fromEnv({}, { OTEL_RESOURCE_ATTRIBUTES: "service.name=from-attrs" })
        .serviceName,
    ).toBe("from-attrs");
    expect(OtelCollectorConfig.fromEnv({}, {}).url).toBe(OtelCollectorConfig.DEFAULT_URL);
  });

  it("routes OTEL_EXPORTER_OTLP_ENDPOINT by the effective protocol", () => {
    const env = { OTEL_EXPORTER_OTLP_ENDPOINT: "http://otel:4317" };
    expect(OtelCollectorConfig.fromEnv({}, env).url).toBe("http://otel:4317");
    const g = OtelCollectorConfig.fromEnv({}, { ...env, OTEL_EXPORTER_OTLP_PROTOCOL: "grpc" });
    expect(g.protocol).toBe("grpc");
    expect(g.grpcUrl).toBe("http://otel:4317");
    expect(g.url).toBe(OtelCollectorConfig.DEFAULT_URL);
    const explicit = OtelCollectorConfig.fromEnv({ protocol: "grpc" }, env);
    expect(explicit.grpcUrl).toBe("http://otel:4317");
    const growProto = OtelCollectorConfig.fromEnv(
      {},
      { ...env, GROW_OTEL_PROTOCOL: "grpc", OTEL_EXPORTER_OTLP_PROTOCOL: "http/protobuf" },
    );
    expect(growProto.protocol).toBe("grpc");
    expect(growProto.grpcUrl).toBe("http://otel:4317");
  });

  it("reads the standard OTEL_* variables", () => {
    const c = OtelCollectorConfig.fromEnv(
      {},
      {
        OTEL_EXPORTER_OTLP_COMPRESSION: "none",
        OTEL_EXPORTER_OTLP_TIMEOUT: "2500",
        OTEL_EXPORTER_OTLP_CERTIFICATE: "/ca.pem",
        OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE: "/c.pem",
        OTEL_EXPORTER_OTLP_CLIENT_KEY: "/k.pem",
        OTEL_EXPORTER_OTLP_LOGS_ENDPOINT: "http://l/v1/logs",
        OTEL_EXPORTER_OTLP_METRICS_ENDPOINT: "http://m/v1/metrics",
        OTEL_EXPORTER_OTLP_LOGS_HEADERS: "x=l",
        OTEL_EXPORTER_OTLP_METRICS_HEADERS: "x=m",
        OTEL_TRACES_SAMPLER: "parentbased_traceidratio",
        OTEL_TRACES_SAMPLER_ARG: "0.1",
        OTEL_PROPAGATORS: "b3multi",
        OTEL_METRIC_EXPORT_INTERVAL: "1000",
        OTEL_BSP_SCHEDULE_DELAY: "100",
        OTEL_BSP_EXPORT_TIMEOUT: "200",
        OTEL_BSP_MAX_QUEUE_SIZE: "300",
        OTEL_BSP_MAX_EXPORT_BATCH_SIZE: "30",
        OTEL_BLRP_SCHEDULE_DELAY: "400",
        OTEL_BLRP_EXPORT_TIMEOUT: "500",
        OTEL_BLRP_MAX_QUEUE_SIZE: "600",
        OTEL_BLRP_MAX_EXPORT_BATCH_SIZE: "60",
        OTEL_LOGS_EXPORTER: "none",
        OTEL_TRACES_EXPORTER: "otlp",
      },
    );
    expect(c).toMatchObject({
      compression: "none",
      exportTimeoutMs: 2500,
      tls: { caCertPath: "/ca.pem", clientCertPath: "/c.pem", clientKeyPath: "/k.pem" },
      logsUrl: "http://l/v1/logs",
      metricsUrl: "http://m/v1/metrics",
      logsHeaders: { x: "l" },
      metricsHeaders: { x: "m" },
      sampler: "parentbased_traceidratio",
      samplerRatio: 0.1,
      propagators: ["b3multi"],
      metricExportIntervalMs: 1000,
      tracesBatch: {
        scheduledDelayMs: 100,
        exportTimeoutMs: 200,
        maxQueueSize: 300,
        maxExportBatchSize: 30,
      },
      logsBatch: {
        flushIntervalMs: 400,
        exportTimeoutMs: 500,
        maxQueueSize: 600,
        maxBatchSize: 60,
      },
      traces: true,
      logs: false,
      metrics: true,
    });
    const off = OtelCollectorConfig.fromEnv(
      {},
      { OTEL_SDK_DISABLED: "TRUE", OTEL_LOGS_EXPORTER: "none" },
    );
    expect([off.traces, off.logs, off.metrics]).toEqual([false, false, false]);
    const on = OtelCollectorConfig.fromEnv({}, { OTEL_SDK_DISABLED: "false" });
    expect(on.traces).toBe(true);
    const noArg = OtelCollectorConfig.fromEnv(
      {},
      { OTEL_TRACES_SAMPLER: "always_on", OTEL_TRACES_SAMPLER_ARG: "0.5" },
    );
    expect(noArg.samplerRatio).toBe(1);
    const none = OtelCollectorConfig.fromEnv(
      {},
      { GROW_OPENTELEMETRY_COLLECTOR_RESOURCE_DETECTORS: "none" },
    );
    expect(none.resourceDetectors).toEqual([]);
  });

  it("an invalid variable throws ConfigError naming it, never its value", () => {
    const cases: Record<string, string> = {
      GROW_OPENTELEMETRY_COLLECTOR_PROTOCOL: "native-secret",
      GROW_OTEL_COMPRESSION: "br-secret",
      GROW_OPENTELEMETRY_COLLECTOR_SAMPLER: "x-secret",
      GROW_OPENTELEMETRY_COLLECTOR_SAMPLER_RATIO: "2-secret",
      GROW_OPENTELEMETRY_COLLECTOR_HEADERS: "novalue-secret",
      GROW_OPENTELEMETRY_COLLECTOR_RESOURCE_DETECTORS: "gcp-secret",
      GROW_OPENTELEMETRY_COLLECTOR_EXPORT_TIMEOUT: "soon-secret",
      OTEL_EXPORTER_OTLP_PROTOCOL: "http/json",
      OTEL_EXPORTER_OTLP_TIMEOUT: "-1",
      OTEL_EXPORTER_OTLP_HEADERS: "a=%E0%A4%A",
      OTEL_EXPORTER_OTLP_TRACES_HEADERS: "=x",
      OTEL_BSP_MAX_QUEUE_SIZE: "0",
      OTEL_TRACES_SAMPLER: "jaeger_remote",
    };
    for (const [name, value] of Object.entries(cases)) {
      let error: unknown;
      try {
        OtelCollectorConfig.fromEnv({}, { [name]: value });
      } catch (err) {
        error = err;
      }
      expect(error, name).toBeInstanceOf(ConfigError);
      expect((error as Error).message).toContain(name);
      expect((error as Error).message).not.toContain("secret");
    }
    expect(() =>
      OtelCollectorConfig.fromEnv(
        {},
        { OTEL_TRACES_SAMPLER: "traceidratio", OTEL_TRACES_SAMPLER_ARG: "abc" },
      ),
    ).toThrow(/OTEL_TRACES_SAMPLER_ARG/);
  });

  it("parseKeyValueList ignores blanks and decodes values", () => {
    expect(parseKeyValueList(" a = 1 ,, b=x%3Dy ", "V")).toEqual({ a: "1", b: "x=y" });
    expect(() => parseKeyValueList("a", "V")).toThrow(/V/);
  });
});
