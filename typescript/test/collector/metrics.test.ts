import { metrics } from "@opentelemetry/api";
import { resourceFromAttributes } from "@opentelemetry/resources";
import {
  AggregationTemporality,
  DataPointType,
  InMemoryMetricExporter,
  MeterProvider,
  type ResourceMetrics,
} from "@opentelemetry/sdk-metrics";
import { afterEach, describe, expect, it } from "vitest";
import {
  ConfigError,
  DEFAULT_METER_NAME,
  OtelCollectorConfig,
  counter,
  createMeterProvider,
  createMetricExporter,
  gauge,
  histogram,
  meter,
  registerRuntimeMetrics,
  upDownCounter,
} from "../../src/index.js";

afterEach(() => {
  metrics.disable();
});

const resource = resourceFromAttributes({ "service.name": "svc-m" });

function points(
  exporter: InMemoryMetricExporter,
): Map<string, ResourceMetrics["scopeMetrics"][number]["metrics"][number]> {
  const out = new Map<string, ResourceMetrics["scopeMetrics"][number]["metrics"][number]>();
  for (const rm of exporter.getMetrics())
    for (const sm of rm.scopeMetrics) for (const m of sm.metrics) out.set(m.descriptor.name, m);
  return out;
}

describe("metrics helpers", () => {
  it("records counter, up/down counter, histogram (boundaries) and gauge", async () => {
    const exporter = new InMemoryMetricExporter(AggregationTemporality.CUMULATIVE);
    const provider = createMeterProvider(
      new OtelCollectorConfig({ metricExportIntervalMs: 60_000 }),
      {
        resource,
        exporter,
      },
    );
    metrics.setGlobalMeterProvider(provider);
    expect(meter()).toBeDefined();
    expect(meter("custom", "1.0")).toBeDefined();
    counter("requests_total", { description: "d", unit: "{request}" }).add(3, { route: "/" });
    upDownCounter("inflight", { meter: "other" }).add(-1);
    histogram("latency", { boundaries: [10, 100], unit: "ms" }).record(5);
    histogram("latency2").record(5);
    gauge("temperature").record(42.5);
    await provider.forceFlush();
    const got = points(exporter);
    const c = got.get("requests_total");
    expect(c?.dataPointType).toBe(DataPointType.SUM);
    expect(c?.descriptor.unit).toBe("{request}");
    expect(c?.dataPoints[0]?.value).toBe(3);
    expect(got.get("inflight")?.dataPoints[0]?.value).toBe(-1);
    const h = got.get("latency");
    expect(h?.dataPointType).toBe(DataPointType.HISTOGRAM);
    expect(
      (h?.dataPoints[0]?.value as { buckets: { boundaries: number[] } }).buckets.boundaries,
    ).toEqual([10, 100]);
    expect(got.get("temperature")?.dataPointType).toBe(DataPointType.GAUGE);
    expect(got.get("temperature")?.dataPoints[0]?.value).toBe(42.5);
    expect(exporter.getMetrics()[0]?.resource.attributes["service.name"]).toBe("svc-m");
    expect(DEFAULT_METER_NAME).toBe("@grow-codes/opentelemetry-collector");
    await provider.shutdown();
  });

  it("validates the export interval", () => {
    expect(() =>
      createMeterProvider(new OtelCollectorConfig(), { resource, exportIntervalMs: 0 }),
    ).toThrow(ConfigError);
  });

  it("creates the OTLP exporter for both protocols", async () => {
    for (const protocol of ["http/protobuf", "grpc"] as const) {
      const e = createMetricExporter(
        new OtelCollectorConfig({ protocol, tls: { insecureSkipVerify: true } }),
      );
      expect(e).toBeDefined();
      await e.shutdown();
    }
  });
});

describe("runtime metrics", () => {
  it("observes memory, cpu, event loop and uptime until stopped", async () => {
    const exporter = new InMemoryMetricExporter(AggregationTemporality.CUMULATIVE);
    const provider = createMeterProvider(new OtelCollectorConfig(), { resource, exporter });
    const handle = registerRuntimeMetrics(provider.getMeter("rt"));
    await new Promise((r) => setTimeout(r, 60));
    await provider.forceFlush();
    const got = points(exporter);
    for (const name of [
      "process.memory.usage",
      "v8js.memory.heap.used",
      "process.cpu.time",
      "process.uptime",
    ])
      expect(got.has(name), name).toBe(true);
    expect(got.get("process.cpu.time")?.dataPoints.length).toBe(2);
    handle.stop();
    handle.stop();
    await provider.shutdown();
  });

  it("works without event-loop monitoring", async () => {
    const provider = new MeterProvider();
    const m = provider.getMeter("x");
    const handle = registerRuntimeMetrics(m);
    handle.stop();
    await provider.shutdown();
  });
});
