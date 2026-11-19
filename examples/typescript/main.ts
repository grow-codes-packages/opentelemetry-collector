// OpenTelemetry Collector TypeScript SDK example: check the collector, then send traces, logs and
// metrics over OTLP with one call (initTelemetry) and shut everything down cleanly.
//
//   (cd ../../typescript && npm install && npm run build)   # or use the published package
//   npm install
//   GROW_OPENTELEMETRY_COLLECTOR_URL=http://127.0.0.1:4318 OTEL_SERVICE_NAME=checkout npm start
//
// Every setting also comes from the standard OTEL_* variables (OTEL_EXPORTER_OTLP_ENDPOINT,
// OTEL_EXPORTER_OTLP_PROTOCOL=grpc, OTEL_EXPORTER_OTLP_HEADERS="authorization=Bearer%20…", …).

import {
  OtelCollectorClient,
  OtelCollectorError,
  traceFields,
  withSpan,
} from "@grow-codes/opentelemetry-collector";

const client = OtelCollectorClient.fromEnv({
  serviceName: process.env.OTEL_SERVICE_NAME ?? "grow-example",
  serviceVersion: "1.0.0",
  // Warnings only; the default logger is silent.
  logger: { debug() {}, info() {}, warn: console.warn, error: console.error },
});
console.log(`config: ${client.config}`); // header values print as [REDACTED]

try {
  // health() never throws for an unreachable collector; it falls back to zpages on stock stacks.
  const health = await client.waitUntilReady(15_000);
  console.log("health:", health);
  console.log(`ping: ${(await client.ping({ signal: AbortSignal.timeout(10_000) })).toFixed(1)} ms`);
  try {
    console.log("collector version:", await client.serverVersion());
  } catch (err) {
    if (!(err instanceof OtelCollectorError && err.code === "UNSUPPORTED")) throw err;
    console.log("collector version: not exposed (zpages disabled)");
  }

  // One call: traces + logs + metrics with a shared resource (host, process, container, k8s).
  const telemetry = client.initTelemetry();
  const log = telemetry.logger("checkout");
  const orders = telemetry.counter("orders_total", { unit: "{order}" });
  const latency = telemetry.histogram("checkout_duration", { unit: "ms", boundaries: [10, 50, 100, 500] });
  const queue = telemetry.gauge("queue_depth");

  await withSpan("checkout", async () => {
    const started = performance.now();
    log.info("order received", { "order.id": "o-42", ...traceFields() });
    await withSpan("charge-card", () => new Promise((r) => setTimeout(r, 25)), { kind: "client" });
    orders.add(1, { "payment.method": "card" });
    queue.record(3);
    latency.record(performance.now() - started);
  }, { kind: "server", attributes: { "order.id": "o-42" } });

  await telemetry.forceFlush();
  console.log("log batch stats:", telemetry.logClient?.stats());
} catch (err) {
  if (err instanceof OtelCollectorError) {
    // Canonical code + HTTP status; the message never contains credentials.
    console.error(`${err.code}: ${err.message}`, { httpStatus: err.httpStatus });
    process.exitCode = 1;
  } else {
    throw err;
  }
} finally {
  await client.close(); // flushes and shuts down the telemetry it created
}
