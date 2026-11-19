// Integration tests: initTelemetry / initTracing / log client / metrics helpers export to the
// real collector; data is read back from the file exporter capture and the Jaeger sidecar.

import assert from "node:assert/strict";
import { before, test } from "node:test";
import {
  OtelCollectorClient,
  OtelCollectorConfig,
  addEvent,
  counter,
  createLogClient,
  currentTraceId,
  detectResourceAttributes,
  gauge,
  histogram,
  initTelemetry,
  initTracing,
  meter,
  setError,
  traceFields,
  upDownCounter,
  withSpan,
} from "@grow-codes/opentelemetry-collector";
import { loadBootstrapEnv, runId } from "./env.ts";
import {
  JaegerReadback,
  capturedLogs,
  capturedMetrics,
  eventually,
  pointsWith,
} from "./readback.ts";

before(() => {
  loadBootstrapEnv();
});

const jaeger = new JaegerReadback();

for (const protocol of ["http/protobuf", "grpc"] as const) {
  test(`initTelemetry exports traces, logs and metrics (${protocol})`, async () => {
    const tag = `${protocol.replace("/", "-")}-${runId}-${Date.now()}`;
    const service = `it-telemetry-${protocol === "grpc" ? "grpc" : "http"}`;
    const handle = initTelemetry({
      config: OtelCollectorConfig.fromEnv({
        protocol,
        serviceName: service,
        serviceVersion: "1.2.3",
        environment: "integration",
        resourceAttributes: { "grow.run": tag },
        runtimeMetrics: true,
      }),
    });
    let traceId = "";
    try {
      const tracer = handle.tracer("it");
      const log = handle.logger("it-logger");
      await withSpan(
        `root-${tag}`,
        async () => {
          traceId = currentTraceId() ?? "";
          addEvent("checkpoint", { step: 1 });
          log.warn(`correlated ${tag}`, { tag });
          assert.equal(traceFields().trace_id, traceId);
          const child = tracer.startSpan("child-op");
          child.end();
          await withSpan("failing-child", () => {
            setError(new Error("boom"));
          });
        },
        { attributes: { tag }, kind: "server", tracer },
      );
      handle.counter("it_requests", { unit: "{request}" }).add(3, { tag });
      handle.gauge("it_temperature").record(42.5, { tag });
      handle.histogram("it_latency", { boundaries: [10, 100], unit: "ms" }).record(5, { tag });
      handle.histogram("it_latency", { boundaries: [10, 100], unit: "ms" }).record(50, { tag });
      handle.histogram("it_latency", { boundaries: [10, 100], unit: "ms" }).record(95, { tag });
      counter("it_global_requests").add(1, { tag });
      upDownCounter("it_inflight").add(2, { tag });
      gauge("it_global_gauge").record(1, { tag });
      histogram("it_global_hist").record(1, { tag });
      assert.ok(meter("it-meter"));
      await handle.forceFlush();
    } finally {
      await handle.shutdown();
      await handle.shutdown();
    }

    // traces: Jaeger read-back (tree, kind, event, error status, resource)
    const trace = await jaeger.waitForTrace(traceId, {
      minSpans: 3,
      timeoutMs: 20_000,
      intervalMs: 250,
    });
    const root = trace.spans.find((s) => s.name === `root-${tag}`);
    assert.ok(root, "root span");
    assert.equal(root.kind, "server");
    assert.equal(root.serviceName, service);
    assert.equal(root.attributes.tag, tag);
    assert.equal(root.events[0]?.name, "checkpoint");
    const child = trace.spans.find((s) => s.name === "child-op");
    assert.equal(child?.parentSpanId, root.spanId);
    const failing = trace.spans.find((s) => s.name === "failing-child");
    assert.equal(failing?.status.code, "error");

    // logs: capture read-back with trace correlation and the shared resource
    const log = await eventually(async () =>
      (await capturedLogs()).find((l) => l.body === `correlated ${tag}`),
    );
    assert.equal(log.record.traceId, traceId);
    assert.equal(log.record.severityText, "WARN");
    assert.equal(log.attributes["logger.name"], "it-logger");
    assert.equal(log.resource["service.name"], service);
    assert.equal(log.resource["service.version"], "1.2.3");
    assert.equal(log.resource["deployment.environment.name"], "integration");
    assert.equal(log.resource["grow.run"], tag);
    assert.equal(typeof log.resource["host.name"], "string");
    assert.equal(typeof log.resource["process.pid"], "number");

    // metrics: counter (monotonic cumulative sum), gauge, histogram buckets, runtime metrics
    const metrics = await eventually(async () => {
      const all = (await capturedMetrics()).filter((m) => m.resource["grow.run"] === tag);
      return all.some((m) => m.metric.name === "it_requests") ? all : undefined;
    });
    const byName = (n: string) => metrics.find((m) => m.metric.name === n)?.metric;
    const requests = byName("it_requests") as {
      sum: { isMonotonic: boolean; aggregationTemporality: number };
    };
    assert.equal(requests.sum.isMonotonic, true);
    assert.equal(requests.sum.aggregationTemporality, 2);
    const point = pointsWith(byName("it_requests") ?? {}, "tag", tag)[0];
    assert.equal(Number(point?.asInt ?? point?.asDouble), 3);
    assert.equal(pointsWith(byName("it_temperature") ?? {}, "tag", tag)[0]?.asDouble, 42.5);
    const hist = pointsWith(byName("it_latency") ?? {}, "tag", tag)[0] as {
      count: string;
      sum: number;
      bucketCounts: string[];
      explicitBounds: number[];
    };
    assert.equal(hist.count, "3");
    assert.equal(hist.sum, 150);
    assert.deepEqual(hist.explicitBounds, [10, 100]);
    assert.deepEqual(hist.bucketCounts, ["1", "2", "0"]);
    for (const n of [
      "it_global_requests",
      "it_inflight",
      "it_global_gauge",
      "it_global_hist",
      "process.memory.usage",
      "process.uptime",
    ])
      assert.ok(byName(n), `metric ${n}`);
  });
}

test("initTracing (standalone and client-owned) exports to the collector over both protocols", async () => {
  for (const protocol of ["http/protobuf", "grpc"] as const) {
    const name = `tracing-${protocol.replace("/", "-")}-${runId}-${Date.now()}`;
    const handle = initTracing({ serviceName: "it-tracing", protocol, setGlobal: false });
    const span = handle.tracer("it").startSpan(name);
    const traceId = span.spanContext().traceId;
    span.end();
    await handle.forceFlush();
    await handle.shutdown();
    const trace = await jaeger.waitForTrace(traceId, { timeoutMs: 20_000, intervalMs: 250 });
    assert.equal(trace.spans[0]?.name, name);
    assert.equal(trace.spans[0]?.serviceName, "it-tracing");
  }
  const client = OtelCollectorClient.fromEnv({ serviceName: "it-client-tracing" });
  const owned = client.initTracing({ setGlobal: false });
  const span = owned.tracer("it").startSpan(`owned-${runId}`);
  const traceId = span.spanContext().traceId;
  span.end();
  await client.close(); // flushes and shuts the handle down
  const trace = await jaeger.waitForTrace(traceId, { timeoutMs: 20_000, intervalMs: 250 });
  assert.equal(trace.spans[0]?.serviceName, "it-client-tracing");
  assert.ok((await jaeger.listServices()).includes("it-client-tracing"));
});

test("client.logClient / createLogClient batch records to the collector", async () => {
  const tag = `logclient-${runId}-${Date.now()}`;
  const client = OtelCollectorClient.fromEnv({ serviceName: "it-logclient", protocol: "grpc" });
  const logs = client.logClient();
  const other = createLogClient(OtelCollectorConfig.fromEnv({ serviceName: "it-createlogclient" }));
  try {
    logs.logger("svc").child({ tag }).error(`client ${tag}`, { password: "hunter2" });
    logs.sendBatch([{ message: `batch-a ${tag}` }, { message: `batch-b ${tag}` }]);
    other.send({ message: `other ${tag}` });
    await logs.flush();
    await other.close();
    assert.equal(logs.stats().sent, 3);
  } finally {
    await client.close();
  }
  const found = await eventually(async () => {
    const all = (await capturedLogs()).filter((l) => l.body.includes(tag));
    return all.length >= 4 ? all : undefined;
  });
  const error = found.find((l) => l.body === `client ${tag}`);
  assert.equal(error?.attributes.password, "[REDACTED]");
  assert.equal(error?.attributes.tag, tag);
  assert.equal(error?.resource["service.name"], "it-logclient");
  assert.equal(
    found.find((l) => l.body === `other ${tag}`)?.resource["service.name"],
    "it-createlogclient",
  );
});

test("client.initTelemetry works against the stock collector (Grow stack config)", async () => {
  const client = new OtelCollectorClient({
    url: process.env.OTELCOL_TEST_STOCK_URL as string,
    serviceName: "it-stock",
    metricExportIntervalMs: 60_000,
  });
  const handle = client.initTelemetry({ setGlobal: false });
  handle.tracer().startSpan("stock-span").end();
  handle.logger().info("stock-log");
  handle.counter("stock_counter").add(1);
  await handle.forceFlush();
  assert.equal(handle.logClient?.stats().sent, 1);
  assert.equal(handle.logClient?.stats().failed, 0);
  await client.close();
  const detected = detectResourceAttributes();
  assert.equal(typeof detected["host.name"], "string");
});
