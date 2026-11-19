import { trace } from "@opentelemetry/api";
import { afterEach, describe, expect, it } from "vitest";
import {
  ConfigError,
  OtelCollectorConfig,
  collectorSpanExporterFactory,
  initTracing,
  resolveTracesEndpoint,
  resolveTracingConfig,
  tracingDefaults,
} from "../../src/index.js";
import { startGrpcServer, type MockGrpcServer } from "../helpers/grpc.js";
import { OTLP_OK, startCollector, type MockCollector } from "./mock.js";

const servers: (MockCollector | MockGrpcServer)[] = [];
afterEach(async () => {
  await Promise.all(servers.splice(0).map((s) => s.close()));
});

describe("resolveTracesEndpoint", () => {
  it("appends /v1/traces only to HTTP bases", () => {
    expect(resolveTracesEndpoint("http/protobuf", "http://c:4318")).toBe("http://c:4318/v1/traces");
    expect(resolveTracesEndpoint("http/protobuf", "http://c:4318/")).toBe(
      "http://c:4318/v1/traces",
    );
    expect(resolveTracesEndpoint("http/protobuf", "http://c/x/v1/traces")).toBe(
      "http://c/x/v1/traces",
    );
    expect(resolveTracesEndpoint("grpc", "http://c:4317")).toBe("http://c:4317");
    expect(resolveTracesEndpoint("http/protobuf", "not a url")).toBe("not a url");
  });
});

describe("initTracing", () => {
  it("exports spans over OTLP/HTTP protobuf with collector defaults", async () => {
    const s = await startCollector(() => ({ body: OTLP_OK }));
    servers.push(s);
    const handle = initTracing(
      { resourceAttributes: { explicit: "yes" }, headers: { "x-explicit": "1" } },
      {
        collector: {
          url: s.url,
          serviceName: "svc-trace",
          serviceVersion: "3.1",
          environment: "test",
          resourceAttributes: { team: "core", explicit: "no" },
          headers: { authorization: "Bearer h" },
          tracesHeaders: { "x-signal": "t" },
          compression: "none",
        },
      },
    );
    expect(handle.endpoint).toBe(`${s.url}/v1/traces`);
    expect(handle.config.serviceName).toBe("svc-trace");
    expect(handle.config.resourceAttributes).toEqual({ team: "core", explicit: "yes" });
    const span = handle.tracer("t").startSpan("op-under-test");
    span.end();
    await handle.forceFlush();
    await handle.shutdown();
    await handle.shutdown();
    const req = s.requests[0];
    expect(req?.url).toBe("/v1/traces");
    expect(req?.headers["content-type"]).toBe("application/x-protobuf");
    expect(req?.headers.authorization).toBe("Bearer h");
    expect(req?.headers["x-signal"]).toBe("t");
    expect(req?.headers["x-explicit"]).toBe("1");
    for (const t of ["op-under-test", "svc-trace", "3.1", "core"])
      expect(req?.body.includes(Buffer.from(t))).toBe(true);
    expect(trace.getTracerProvider()).toBeDefined();
  });

  it("exports over gRPC and resolves endpoints for protocol overrides", async () => {
    const g = await startGrpcServer();
    servers.push(g);
    const handle = initTracing(
      { serviceName: "svc-grpc", protocol: "grpc", setGlobal: false },
      { collector: { grpcUrl: g.url, url: "http://127.0.0.1:9" } },
    );
    expect(handle.endpoint).toBe(g.url);
    handle.tracer("t").startSpan("grpc-span").end();
    await handle.forceFlush();
    await handle.shutdown();
    expect(g.calls[0]?.path).toBe("/opentelemetry.proto.collector.trace.v1.TraceService/Export");
    expect(g.calls[0]?.body.includes(Buffer.from("grpc-span"))).toBe(true);

    const back = initTracing(
      { protocol: "http/protobuf", setGlobal: false },
      { collector: { protocol: "grpc", url: "http://h:4318" } },
    );
    expect(back.endpoint).toBe("http://h:4318/v1/traces");
    await back.shutdown();
    const explicit = initTracing(
      { endpoint: "http://e:4318", setGlobal: false, serviceName: " " },
      { env: false },
    );
    expect(explicit.endpoint).toBe("http://e:4318/v1/traces");
    expect(explicit.config.serviceName).toMatch(/^unknown_service:/);
    await explicit.shutdown();
  });

  it("reads the environment unless env is false", async () => {
    const h = initTracing(
      { setGlobal: false },
      {
        env: {
          OTEL_SERVICE_NAME: "from-env",
          OTEL_EXPORTER_OTLP_PROTOCOL: "grpc",
          OTEL_EXPORTER_OTLP_ENDPOINT: "http://g:4317",
          OTEL_TRACES_SAMPLER: "always_off",
        },
      },
    );
    expect(h.config.serviceName).toBe("from-env");
    expect(h.config.protocol).toBe("grpc");
    expect(h.endpoint).toBe("http://g:4317");
    expect(h.config.sampler).toBe("always_off");
    await h.shutdown();
    const saved = process.env.OTEL_SERVICE_NAME;
    process.env.OTEL_SERVICE_NAME = "process-env";
    try {
      const p = initTracing({ setGlobal: false });
      expect(p.config.serviceName).toBe("process-env");
      await p.shutdown();
    } finally {
      if (saved === undefined) delete process.env.OTEL_SERVICE_NAME;
      else process.env.OTEL_SERVICE_NAME = saved;
    }
  });

  it("rejects the native protocol", () => {
    expect(() => initTracing({ protocol: "native", setGlobal: false }, { env: false })).toThrow(
      ConfigError,
    );
  });
});

describe("collectorSpanExporterFactory / tracingDefaults", () => {
  it("builds exporters for both protocols, with and without an endpoint", async () => {
    for (const protocol of ["http/protobuf", "grpc"] as const) {
      const resolved = resolveTracingConfig({
        serviceName: "s",
        protocol,
        tls: { insecureSkipVerify: true },
      });
      const exporter = collectorSpanExporterFactory(resolved);
      expect(exporter).toBeDefined();
      await exporter.shutdown();
    }
    const native = resolveTracingConfig({ serviceName: "s", protocol: "native" });
    expect(() => collectorSpanExporterFactory(native)).toThrow(/no native protocol/);
  });

  it("maps every collector field", () => {
    const d = tracingDefaults(
      new OtelCollectorConfig({
        tracesBatch: { maxQueueSize: 10, maxExportBatchSize: 5 },
        exportTimeoutMs: 1234,
      }),
    );
    expect(d.batch).toEqual({ exportTimeoutMs: 1234, maxQueueSize: 10, maxExportBatchSize: 5 });
    expect(d.serviceVersion).toBeUndefined();
    expect(d.endpoint).toBe("http://127.0.0.1:4318/v1/traces");
  });
});
