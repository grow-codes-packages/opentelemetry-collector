import { metrics, trace } from "@opentelemetry/api";
import { AggregationTemporality, InMemoryMetricExporter } from "@opentelemetry/sdk-metrics";
import { afterEach, describe, expect, it } from "vitest";
import {
  ConfigError,
  OtelCollectorConfig,
  Severity,
  TimeoutError,
  counter as globalCounter,
  initTelemetry,
  telemetryResourceAttributes,
} from "../../src/index.js";
import { OTLP_OK, startCollector, type MockCollector } from "./mock.js";

const servers: MockCollector[] = [];
afterEach(async () => {
  await Promise.all(servers.splice(0).map((s) => s.close()));
});

async function collector(): Promise<MockCollector> {
  const s = await startCollector(() => ({ body: OTLP_OK }));
  servers.push(s);
  return s;
}

describe("initTelemetry", () => {
  it("exports traces, logs and metrics with one resource and registers globals", async () => {
    const s = await collector();
    const h = initTelemetry({
      config: {
        url: s.url,
        serviceName: "svc-all",
        serviceVersion: "9.9",
        environment: "ci",
        resourceAttributes: { team: "core" },
        compression: "none",
        runtimeMetrics: true,
        resourceDetectors: ["host", "k8s"],
      },
      detect: { env: { K8S_POD_NAME: "pod-1" } },
    });
    expect(h.resourceAttributes).toMatchObject({
      "service.name": "svc-all",
      "service.version": "9.9",
      "deployment.environment.name": "ci",
      team: "core",
      "k8s.pod.name": "pod-1",
    });
    expect(h.resource.attributes["telemetry.sdk.language"]).toBe("nodejs");
    expect(h.tracerProvider).toBeDefined();
    expect(h.meterProvider).toBeDefined();
    expect(h.logClient).toBeDefined();
    h.tracer().startSpan("telemetry-span").end();
    h.tracer("named", "1").startSpan("named-span").end();
    h.logger("app", { a: 1 }).warn("telemetry-log");
    h.counter("telemetry_count").add(2);
    h.gauge("telemetry_gauge").record(1.5);
    h.histogram("telemetry_hist", { boundaries: [1] }).record(3);
    h.upDownCounter("telemetry_ud").add(1);
    expect(h.meter("m")).toBeDefined();
    globalCounter("global_count").add(1);
    await h.forceFlush();
    const paths = new Set(s.requests.map((r) => r.url));
    expect(paths).toEqual(new Set(["/v1/traces", "/v1/logs", "/v1/metrics"]));
    const metricsBody = Buffer.concat(
      s.requests.filter((r) => r.url === "/v1/metrics").map((r) => r.body),
    );
    for (const name of [
      "telemetry_count",
      "telemetry_gauge",
      "global_count",
      "process.memory.usage",
      "svc-all",
    ])
      expect(metricsBody.includes(Buffer.from(name)), name).toBe(true);
    const logBody = s.requests.find((r) => r.url === "/v1/logs")?.body;
    expect(logBody?.includes(Buffer.from("telemetry-log"))).toBe(true);
    expect(logBody?.includes(Buffer.from("pod-1"))).toBe(true);
    await h.shutdown();
    await h.shutdown();
    expect(metrics.getMeterProvider()).not.toBe(h.meterProvider);
  });

  it("honours disabled signals with no-op tracer, meter and logger", async () => {
    const h = initTelemetry({
      env: { OTEL_SDK_DISABLED: "true" },
      setGlobal: false,
    });
    expect(h.tracing).toBeUndefined();
    expect(h.logClient).toBeUndefined();
    expect(h.meterProvider).toBeUndefined();
    const span = h.tracer("x").startSpan("noop");
    expect(span.isRecording()).toBe(false);
    span.end();
    h.counter("c").add(1);
    h.logger().info("dropped");
    h.logger("n", { a: 1 }).info("dropped");
    await h.forceFlush();
    await h.shutdown();
    const t = initTelemetry({
      config: { traces: false },
      traces: true,
      logs: false,
      metrics: false,
      setGlobal: false,
    });
    expect(t.tracing).toBeDefined();
    await t.shutdown();
  });

  it("applies per-signal options", async () => {
    const s = await collector();
    const exporter = new InMemoryMetricExporter(AggregationTemporality.CUMULATIVE);
    const h = initTelemetry({
      config: new OtelCollectorConfig({ url: s.url, serviceName: "svc-o" }),
      setGlobal: false,
      traces: { sampler: "always_off", init: {} },
      logs: { batch: { maxBatchSize: 1 }, redaction: false, minSeverity: Severity.Trace },
      metrics: { exportIntervalMs: 1000, runtimeMetrics: false, exporter },
    });
    expect(h.tracing?.config.sampler).toBe("always_off");
    h.logger().debug("debug-visible", { password: "kept" });
    h.counter("c").add(1);
    await h.forceFlush();
    expect(exporter.getMetrics().length).toBeGreaterThan(0);
    const logBody = s.requests.find((r) => r.url === "/v1/logs")?.body;
    expect(logBody?.includes(Buffer.from("kept"))).toBe(true);
    expect(trace.getTracerProvider()).not.toBe(h.tracerProvider);
    await h.shutdown();
  });

  it("rejects invalid settings and cleans up", () => {
    expect(() =>
      initTelemetry({ env: false, traces: { protocol: "native" }, setGlobal: false }),
    ).toThrow(ConfigError);
  });

  it("forceFlush times out and shutdown surfaces failures", async () => {
    const s = await startCollector(() => ({ body: OTLP_OK, delayMs: 400 }));
    servers.push(s);
    const h = initTelemetry({
      config: { url: s.url, metrics: false, traces: false },
      setGlobal: false,
    });
    h.logger().info("slow");
    await expect(h.forceFlush(20)).rejects.toBeInstanceOf(TimeoutError);
    await h.shutdown();
    const bad = initTelemetry({ config: { metrics: false, logs: false }, setGlobal: false });
    const tracing = bad.tracing;
    if (tracing === undefined) throw new Error("tracing expected");
    (tracing as { shutdown: () => Promise<void> }).shutdown = () =>
      Promise.reject(new Error("boom"));
    await expect(bad.shutdown()).rejects.toThrow("boom");
  });

  it("telemetryResourceAttributes layers detected < config < service fields", () => {
    const attrs = telemetryResourceAttributes(
      new OtelCollectorConfig({
        resourceAttributes: { "host.name": "override" },
        serviceName: "s",
      }),
      { detectors: ["host"] },
    );
    expect(attrs["host.name"]).toBe("override");
    expect(attrs["service.name"]).toBe("s");
    expect(attrs["deployment.environment.name"]).toBeUndefined();
  });

  it("uses process.env by default", async () => {
    const saved = process.env.OTEL_SDK_DISABLED;
    process.env.OTEL_SDK_DISABLED = "true";
    try {
      const h = initTelemetry();
      expect(h.tracing).toBeUndefined();
      await h.shutdown();
    } finally {
      if (saved === undefined) delete process.env.OTEL_SDK_DISABLED;
      else process.env.OTEL_SDK_DISABLED = saved;
    }
  });
});
