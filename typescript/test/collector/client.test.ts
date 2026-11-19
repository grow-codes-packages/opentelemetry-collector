import { afterEach, describe, expect, it } from "vitest";
import {
  AuthenticationError,
  CancelledError,
  CollectorHealthClient,
  ConfigError,
  NotFoundError,
  OtelCollectorClient,
  OtelCollectorConfig,
  OtelCollectorError,
  TimeoutError,
  UnavailableError,
  UnsupportedError,
  parseServicezVersion,
  parseTargetInfoVersion,
} from "../../src/index.js";
import { closedPort } from "../helpers/server.js";
import { startGrpcServer, type MockGrpcServer } from "../helpers/grpc.js";
import {
  OTLP_OK,
  fixture,
  fixtureResponse,
  startCollector,
  type MockCollector,
  type RawRequest,
  type RawResponse,
} from "./mock.js";

const servers: (MockCollector | MockGrpcServer)[] = [];
afterEach(async () => {
  await Promise.all(servers.splice(0).map((s) => s.close()));
});

const fast = { retry: { maxRetries: 1, initialBackoffMs: 1 } };

async function collector(
  handler: (req: RawRequest, i: number) => RawResponse,
): Promise<MockCollector> {
  const s = await startCollector(handler);
  servers.push(s);
  return s;
}

/** One mock server playing every collector port (health at `/`, zpages at `/debug/servicez`). */
async function fullCollector(overrides: Record<string, RawResponse> = {}): Promise<MockCollector> {
  return collector((req) => {
    const path = req.url.split("?")[0] ?? "";
    if (overrides[path] !== undefined) return overrides[path];
    if (path === "/") return fixtureResponse("health.ok");
    if (path === "/debug/servicez") return fixtureResponse("serverVersion.zpages");
    if (path === "/metrics") return fixtureResponse("serverVersion.telemetry_target_info");
    if (["/v1/traces", "/v1/logs", "/v1/metrics"].includes(path) && req.method === "POST")
      return req.headers["content-type"] === "application/json"
        ? { body: { partialSuccess: {} } }
        : { body: OTLP_OK, headers: { "content-type": "application/x-protobuf" } };
    return fixtureResponse("export.http_not_found");
  });
}

function clientFor(
  s: MockCollector,
  extra: ConstructorParameters<typeof OtelCollectorClient>[0] = {},
) {
  return new OtelCollectorClient({
    url: s.url,
    healthUrl: `${s.url}/`,
    zpagesUrl: s.url,
    telemetryUrl: `${s.url}/metrics`,
    ...fast,
    ...extra,
  });
}

describe("health", () => {
  it("maps the health_check fixture to ok with details", async () => {
    const s = await fullCollector();
    const c = clientFor(s);
    const h = await c.health();
    expect(h).toEqual({
      healthy: true,
      status: "ok",
      details: {
        source: "health_check",
        vendorStatus: "Server available",
        upSince: fixture("health.ok").response.body?.json
          ? (fixture("health.ok").response.body?.json as { upSince: string }).upSince
          : "",
        uptime: (fixture("health.ok").response.body?.json as { uptime: string }).uptime,
      },
    });
    expect(s.requests[0]?.headers["user-agent"]).toMatch(
      /^grow-codes-opentelemetry-collector\/\S+ \(typescript\)$/,
    );
    await c.close();
  });

  it("reports 503 as unhealthy and never sends export headers to admin ports", async () => {
    const s = await fullCollector({
      "/": { status: 503, body: { status: "Server not available" } },
    });
    const c = clientFor(s, { headers: { authorization: "Bearer secret-x" } });
    expect(await c.health()).toEqual({
      healthy: false,
      status: "unhealthy",
      details: { source: "health_check", httpStatus: "503" },
    });
    expect(s.requests[0]?.headers.authorization).toBeUndefined();
    const other = await fullCollector({ "/": { status: 500 } });
    const h = await clientFor(other, { retry: { maxRetries: 0 } }).health();
    expect(h.status).toBe("unhealthy");
  });

  it("falls back to zpages when the health endpoint is unreachable (stock stacks)", async () => {
    const s = await fullCollector();
    const port = await closedPort();
    const c = clientFor(s, { healthUrl: `http://127.0.0.1:${port}/` });
    expect(await c.health()).toEqual({
      healthy: true,
      status: "ok",
      details: { source: "zpages" },
    });
    const none = new CollectorHealthClient({
      url: `http://127.0.0.1:${port}`,
      retry: { maxRetries: 0 },
    });
    const h = await none.health();
    expect(h.healthy).toBe(false);
    expect(h.status).toBe("unreachable");
    await none.close();
  });

  it("throws CANCELLED for an aborted signal", async () => {
    const s = await fullCollector();
    await expect(clientFor(s).health({ signal: AbortSignal.abort() })).rejects.toBeInstanceOf(
      CancelledError,
    );
  });

  it("waitUntilReady polls until healthy, else TimeoutError", async () => {
    let calls = 0;
    const s = await collector((req) => {
      if (req.url === "/") return calls++ < 2 ? { status: 503 } : fixtureResponse("health.ok");
      return { status: 404 };
    });
    const c = clientFor(s);
    const h = await c.waitUntilReady(5000, { intervalMs: 10 });
    expect(h.healthy).toBe(true);
    expect(calls).toBe(3);
    const down = await collector(() => ({ status: 503 }));
    await expect(clientFor(down).waitUntilReady(50, { intervalMs: 10 })).rejects.toBeInstanceOf(
      TimeoutError,
    );
    await expect(
      clientFor(down).waitUntilReady(1000, { signal: AbortSignal.abort() }),
    ).rejects.toBeInstanceOf(CancelledError);
    const hc = CollectorHealthClient.fromEnv({ url: s.url, healthUrl: `${s.url}/`, ...fast });
    expect((await hc.waitUntilReady()).healthy).toBe(true);
    await hc.close();
  });
});

describe("health edge cases", () => {
  it("accepts an empty or non-standard 200 body", async () => {
    const s = await fullCollector({ "/": { body: "" } });
    expect(await clientFor(s).health()).toEqual({
      healthy: true,
      status: "ok",
      details: { source: "health_check" },
    });
    const odd = await fullCollector({ "/": { body: { status: 1, extra: "x" } } });
    expect((await clientFor(odd).health()).details).toEqual({ source: "health_check", extra: "x" });
  });
});

describe("serverVersion", () => {
  it("parses zpages servicez (fixture) and the stock page", async () => {
    expect(parseServicezVersion(fixtureResponse("serverVersion.zpages").body as string)).toBe(
      "0.165.0",
    );
    expect(parseServicezVersion(fixtureResponse("serverVersion.zpages_stock").body as string)).toBe(
      "0.165.0",
    );
    expect(parseServicezVersion("<html></html>")).toBeUndefined();
    const s = await fullCollector();
    expect(await clientFor(s).serverVersion()).toBe("0.165.0");
  });

  it("falls back to target_info, else UnsupportedError", async () => {
    const metrics =
      'x 1\ntarget_info{service_name="otelcol-contrib",service_version="0.165.0"} 1\n';
    expect(parseTargetInfoVersion(metrics)).toBe("0.165.0");
    expect(parseTargetInfoVersion("x 1")).toBeUndefined();
    const s = await fullCollector({
      "/debug/servicez": { status: 404 },
      "/metrics": { body: metrics },
    });
    expect(await clientFor(s).serverVersion()).toBe("0.165.0");
    const noVersion = await fullCollector({
      "/debug/servicez": { body: "<html>no version</html>" },
      "/metrics": { body: "x 1" },
    });
    await expect(clientFor(noVersion).serverVersion()).rejects.toBeInstanceOf(UnsupportedError);
    const port = await closedPort();
    const down = new OtelCollectorClient({
      url: `http://127.0.0.1:${port}`,
      telemetryUrl: `http://127.0.0.1:${port}/metrics`,
      retry: { maxRetries: 0 },
    });
    await expect(down.serverVersion()).rejects.toBeInstanceOf(UnsupportedError);
    const noTelemetry = new OtelCollectorClient({
      url: `http://127.0.0.1:${port}`,
      retry: { maxRetries: 0 },
    });
    await expect(noTelemetry.serverVersion()).rejects.toThrow(/exposes no version/);
  });

  it("propagates cancellation", async () => {
    const s = await fullCollector();
    await expect(
      clientFor(s).serverVersion({ signal: AbortSignal.abort() }),
    ).rejects.toBeInstanceOf(CancelledError);
    const controller = new AbortController();
    const slow = await fullCollector({
      "/debug/servicez": { status: 404 },
      "/metrics": { delayMs: 500 },
    });
    const p = clientFor(slow).serverVersion({ signal: controller.signal });
    setTimeout(() => controller.abort(), 50);
    await expect(p).rejects.toBeInstanceOf(CancelledError);
  });
});

describe("ping (HTTP)", () => {
  it("sends an empty OTLP export with the configured headers", async () => {
    const s = await fullCollector();
    const c = clientFor(s, {
      headers: { authorization: "Bearer good" },
      tracesHeaders: { "x-signal": "traces" },
    });
    const latency = await c.ping();
    expect(latency).toBeGreaterThanOrEqual(0);
    const req = s.requests.find((r) => r.url === "/v1/traces");
    expect(req?.method).toBe("POST");
    expect(req?.headers["content-type"]).toBe("application/json");
    expect(req?.body.toString()).toBe("{}");
    expect(req?.headers.authorization).toBe("Bearer good");
    expect(req?.headers["x-signal"]).toBe("traces");
  });

  it("maps 401 (protobuf and JSON bodies) to AuthenticationError without leaking headers", async () => {
    const secret = "very-secret-token";
    for (const name of [
      "ping.http_unauthenticated_missing_token",
      "ping.http_unauthenticated_wrong_token",
    ]) {
      const s = await fullCollector({ "/v1/traces": fixtureResponse(name) });
      const c = clientFor(s, { headers: { authorization: `Bearer ${secret}` } });
      const err = await c.ping().catch((e: unknown) => e);
      expect(err).toBeInstanceOf(AuthenticationError);
      expect((err as OtelCollectorError).httpStatus).toBe(401);
      expect((err as Error).message).toMatch(/^ping: .*\(HTTP 401\)$/);
      expect((err as Error).message).not.toContain(secret);
    }
    const json = await fullCollector({
      "/v1/traces": {
        status: 401,
        body: { code: 16, message: `missing or empty authorization header: Authorization` },
      },
    });
    await expect(clientFor(json).ping()).rejects.toThrow(
      "ping: missing or empty authorization header: Authorization (HTTP 401)",
    );
  });

  it("maps a wrong path to NotFoundError and retries 503", async () => {
    const s = await fullCollector();
    await expect(clientFor(s, { tracesUrl: `${s.url}/v1/nope` }).ping()).rejects.toBeInstanceOf(
      NotFoundError,
    );
    let n = 0;
    const flakyServer = await collector(() => (n++ === 0 ? { status: 503 } : { body: OTLP_OK }));
    const c2 = new OtelCollectorClient({ url: flakyServer.url, ...fast });
    expect(await c2.ping()).toBeGreaterThanOrEqual(0);
    expect(n).toBe(2);
  });
});

describe("ping (gRPC)", () => {
  it("calls TraceService/Export with an empty message and metadata", async () => {
    const g = await startGrpcServer();
    servers.push(g);
    const c = new OtelCollectorClient({
      protocol: "grpc",
      grpcUrl: g.url,
      headers: { Authorization: "Bearer good" },
    });
    expect(await c.ping()).toBeGreaterThanOrEqual(0);
    expect(g.calls[0]?.path).toBe("/opentelemetry.proto.collector.trace.v1.TraceService/Export");
    expect(g.calls[0]?.body.length).toBe(0);
    expect(g.calls[0]?.metadata.get("authorization")).toEqual(["Bearer good"]);
    await c.close();
  });

  it("maps gRPC statuses (UNAUTHENTICATED fixture, UNAVAILABLE retried)", async () => {
    const message = fixture("ping.grpc_unauthenticated_wrong_token") as unknown as {
      response: { grpcMessage: string };
    };
    const g = await startGrpcServer(() => ({ code: 16, details: message.response.grpcMessage }));
    servers.push(g);
    const c = new OtelCollectorClient({ protocol: "grpc", grpcUrl: g.url, ...fast });
    const err = await c.ping().catch((e: unknown) => e);
    expect(err).toBeInstanceOf(AuthenticationError);
    expect((err as Error).message).toBe(
      "ping: provided authorization does not match expected scheme or token (gRPC UNAUTHENTICATED)",
    );
    let n = 0;
    const flaky = await startGrpcServer(() => (n++ === 0 ? { code: 14, details: "busy" } : {}));
    servers.push(flaky);
    const c2 = new OtelCollectorClient({ protocol: "grpc", grpcUrl: flaky.url, ...fast });
    expect(await c2.ping()).toBeGreaterThanOrEqual(0);
    expect(n).toBe(2);
    const down = await startGrpcServer(() => ({ code: 14 }));
    servers.push(down);
    const c3 = new OtelCollectorClient({
      protocol: "grpc",
      grpcUrl: down.url,
      retry: { maxRetries: 0 },
    });
    await expect(c3.ping()).rejects.toBeInstanceOf(UnavailableError);
  });

  it("handles deadline, cancellation and abort during backoff", async () => {
    const g = await startGrpcServer(() => ({ delayMs: 300 }));
    servers.push(g);
    const slow = new OtelCollectorClient({
      protocol: "grpc",
      grpcUrl: g.url,
      requestTimeoutMs: 50,
    });
    await expect(slow.ping()).rejects.toBeInstanceOf(TimeoutError);
    const c = new OtelCollectorClient({ protocol: "grpc", grpcUrl: g.url });
    await expect(c.ping({ signal: AbortSignal.abort() })).rejects.toBeInstanceOf(CancelledError);
    const controller = new AbortController();
    const p = c.ping({ signal: controller.signal });
    setTimeout(() => controller.abort(), 30);
    await expect(p).rejects.toBeInstanceOf(CancelledError);
    const busy = await startGrpcServer(() => ({ code: 14 }));
    servers.push(busy);
    const backoff = new OtelCollectorClient(
      { protocol: "grpc", grpcUrl: busy.url, retry: { maxRetries: 3, initialBackoffMs: 1000 } },
      {
        sleep: (_ms, signal) =>
          new Promise((_, reject) => {
            signal?.addEventListener("abort", () => reject(new Error("aborted")));
          }),
      },
    );
    const c2 = new AbortController();
    const p2 = backoff.ping({ signal: c2.signal });
    setTimeout(() => c2.abort(), 50);
    await expect(p2).rejects.toBeInstanceOf(CancelledError);
  });
});

describe("client lifecycle", () => {
  it("fromEnv, query clients and transport accessor", async () => {
    const c = OtelCollectorClient.fromEnv({ url: "http://127.0.0.1:1" });
    expect(c.config).toBeInstanceOf(OtelCollectorConfig);
    expect(c.transport.userAgent).toContain("(typescript)");
    expect(c.healthClient).toBeInstanceOf(CollectorHealthClient);
    await expect(c.traceQuery().getTrace("a".repeat(32))).rejects.toBeInstanceOf(UnsupportedError);
    await expect(c.logQuery().search("x")).rejects.toBeInstanceOf(UnsupportedError);
    await c.close();
  });

  it("close() shuts down owned handles and is idempotent", async () => {
    const s = await fullCollector();
    const c = clientFor(s, { metrics: false });
    const exporter = c.logExporter();
    const logs = c.logClient();
    const tracing = c.initTracing({ serviceName: "svc" }, {});
    const telemetry = c.initTelemetry({ setGlobal: false, metrics: false });
    logs.send({ message: "bye" });
    await c.close();
    await c.close();
    expect(logs.stats().sent).toBe(1);
    expect((await exporter.export([], { gzip: false })).accepted).toBe(0);
    await tracing.shutdown();
    await telemetry.shutdown();
    expect(() => c.logClient()).toThrow(OtelCollectorError);
    expect(() => c.logClient()).toThrow(/closed/);
    await expect(c.ping()).rejects.toBeInstanceOf(ConfigError);
  });
});
