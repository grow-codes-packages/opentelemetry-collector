import { resourceFromAttributes } from "@opentelemetry/resources";
import { afterEach, describe, expect, it } from "vitest";
import {
  AuthenticationError,
  CancelledError,
  OtelCollectorConfig,
  OtlpLogExporter,
  Severity,
  UnavailableError,
  ValidationError,
  createLogClient,
  toReadableLogRecord,
  type LogRecord,
} from "../../src/index.js";
import { startGrpcServer, type MockGrpcServer } from "../helpers/grpc.js";
import { closedPort } from "../helpers/server.js";
import {
  OTLP_OK,
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

async function collector(
  handler: ((req: RawRequest, i: number) => RawResponse) | RawResponse[],
): Promise<MockCollector> {
  const s = await startCollector(handler);
  servers.push(s);
  return s;
}

function record(body: string, extra: Partial<LogRecord> = {}): LogRecord {
  return {
    timestamp: new Date("2026-11-18T10:00:00.123Z"),
    severity: Severity.Warn,
    severityText: "WARN",
    body,
    attributes: { run: "r1", n: 2, nested: { a: true } },
    resource: { "service.name": "svc" },
    ...extra,
  };
}

/** `partial_success{rejected:2, error_message:"x"}` (API.md §7). */
const PARTIAL = Buffer.from("0a050802120178", "hex");

describe("OtlpLogExporter (HTTP)", () => {
  it("POSTs protobuf to /v1/logs (recorded success body) with headers and gzip", async () => {
    const s = await collector(() => fixtureResponse("exportLogs.http_protobuf_ok"));
    const ex = new OtlpLogExporter({
      url: s.url,
      headers: { authorization: "Bearer t" },
      logsHeaders: { "x-logs": "1" },
    });
    expect(ex.endpoint).toBe(`${s.url}/v1/logs`);
    const result = await ex.export(
      [
        record("hello-1", {
          traceId: "4bf92f3577b34da6a3ce929d0e0e4736",
          spanId: "00f067aa0ba902b7",
        }),
        record("hello-2", { resource: { "service.name": "svc" } }),
        record("hello-3", { resource: { "service.name": "other" } }),
      ],
      { gzip: true },
    );
    expect(result).toEqual({ accepted: 3, rejected: 0 });
    const req = s.requests[0] as RawRequest;
    expect(req.url).toBe("/v1/logs");
    expect(req.headers["content-type"]).toBe("application/x-protobuf");
    expect(req.headers.authorization).toBe("Bearer t");
    expect(req.headers["x-logs"]).toBe("1");
    expect(req.headers["user-agent"]).toMatch(
      /^grow-codes-opentelemetry-collector\/\S+ \(typescript\) OTel/,
    );
    expect(req.gzip).toBe(true);
    for (const text of ["hello-1", "hello-2", "hello-3", "svc", "other", "WARN", "nested"])
      expect(req.body.includes(Buffer.from(text))).toBe(true);
    expect(req.body.includes(Buffer.from("4bf92f3577b34da6a3ce929d0e0e4736", "hex"))).toBe(true);
    const plain = await ex.export([record("x")], { gzip: false });
    expect(plain.accepted).toBe(1);
    expect(s.requests[1]?.gzip).toBe(false);
    expect(await ex.export([], { gzip: true })).toEqual({ accepted: 0, rejected: 0 });
    await ex.shutdown();
    await ex.shutdown();
    const after = await ex.export([record("late")], { gzip: true });
    expect(after.error).toBeInstanceOf(ValidationError);
  });

  it("reports a partial success as rejected records", async () => {
    const s = await collector([{ body: PARTIAL }]);
    const ex = new OtlpLogExporter({ url: s.url, compression: "none" });
    const result = await ex.export([record("a"), record("b"), record("c")], { gzip: true });
    expect(result.accepted).toBe(1);
    expect(result.rejected).toBe(2);
    expect(result.error).toBeInstanceOf(ValidationError);
    expect(result.error?.message).toBe("logs.export: the collector rejected 2 of 3 records (x)");
    expect(result.error?.retryable).toBe(false);
    const noMsg = await collector([{ body: Buffer.from("0a020801", "hex") }]);
    const r2 = await new OtlpLogExporter({ url: noMsg.url }).export([record("a")], { gzip: false });
    expect(r2.error?.message).toBe("logs.export: the collector rejected 1 of 1 records");
  });

  it("maps HTTP failures (recorded 401/400/404 bodies) to SDK errors", async () => {
    const secret = "tok-123456";
    const s = await collector([fixtureResponse("ping.http_unauthenticated_missing_token")]);
    const r = await new OtlpLogExporter({
      url: s.url,
      headers: { authorization: `Bearer ${secret}` },
    }).export([record("a")], { gzip: false });
    expect(r).toMatchObject({ accepted: 0, rejected: 1 });
    expect(r.error).toBeInstanceOf(AuthenticationError);
    expect(r.error?.message).toBe(
      "logs.export: missing or empty authorization header: Authorization (HTTP 401)",
    );
    expect(r.error?.retryable).toBe(false);
    const bad = await collector([fixtureResponse("export.http_bad_request_json")]);
    const r2 = await new OtlpLogExporter({ url: bad.url }).export([record("a")], { gzip: false });
    expect(r2.error).toBeInstanceOf(ValidationError);
    expect(r2.error?.message).toMatch(/^logs\.export: ReadObject/);
    const nf = await collector([fixtureResponse("export.http_not_found")]);
    const r3 = await new OtlpLogExporter({ url: nf.url }).export([record("a")], { gzip: false });
    expect(r3.error?.code).toBe("NOT_FOUND");
    expect(r3.error?.message).toBe("logs.export: 404 page not found (HTTP 404)");
  });

  it("lets the official exporter retry 503 (OTLP rules), then reports UNAVAILABLE", async () => {
    const s = await collector([
      { status: 503, headers: { "retry-after": "0" } },
      { body: OTLP_OK },
    ]);
    const ok = await new OtlpLogExporter({ url: s.url }).export([record("a")], { gzip: false });
    expect(ok.accepted).toBe(1);
    expect(s.requests).toHaveLength(2);
    const down = await collector([{ status: 503 }]);
    const r = await new OtlpLogExporter({ url: down.url, exportTimeoutMs: 300 }).export(
      [record("a")],
      { gzip: false },
    );
    expect(r.error).toBeInstanceOf(UnavailableError);
    expect(r.error?.retryable).toBe(false);
  });

  it("maps network errors and honours cancellation", async () => {
    const port = await closedPort();
    const r = await new OtlpLogExporter({
      url: `http://127.0.0.1:${port}`,
      exportTimeoutMs: 200,
    }).export([record("a")], { gzip: false });
    expect(r.error?.code).toMatch(/UNAVAILABLE|TIMEOUT/);
    const slow = await collector([{ delayMs: 300, body: OTLP_OK }]);
    const ex = new OtlpLogExporter({ url: slow.url });
    const pre = await ex.export([record("a")], { gzip: false, signal: AbortSignal.abort() });
    expect(pre.error).toBeInstanceOf(CancelledError);
    const controller = new AbortController();
    const p = ex.export([record("a")], { gzip: false, signal: controller.signal });
    setTimeout(() => controller.abort(), 20);
    expect((await p).error).toBeInstanceOf(CancelledError);
    const done = await ex.export([record("b")], {
      gzip: false,
      signal: new AbortController().signal,
    });
    expect(done.accepted).toBe(1);
    await ex.shutdown();
  });

  it("reports an unreadable TLS file as CONFIG", async () => {
    const ex = new OtlpLogExporter({ tls: { caCertPath: "/nonexistent/ca.pem" } });
    const r = await ex.export([record("a")], { gzip: false });
    expect(r.error?.code).toBe("CONFIG");
  });
});

describe("OtlpLogExporter (gRPC)", () => {
  it("calls LogsService/Export with metadata; maps partial success and statuses", async () => {
    const replies = [{ body: OTLP_OK }, { body: PARTIAL }, { code: 16, details: "bad token" }];
    const g = await startGrpcServer((_c, i) => replies[i] ?? {});
    servers.push(g);
    const ex = new OtlpLogExporter({
      protocol: "grpc",
      grpcUrl: g.url,
      headers: { "X-Team": "core" },
      compression: "none",
    });
    expect(ex.endpoint).toBe(g.url);
    expect(await ex.export([record("g1")], { gzip: true })).toEqual({ accepted: 1, rejected: 0 });
    expect(g.calls[0]?.path).toBe("/opentelemetry.proto.collector.logs.v1.LogsService/Export");
    expect(g.calls[0]?.metadata.get("x-team")).toEqual(["core"]);
    expect(g.calls[0]?.body.includes(Buffer.from("g1"))).toBe(true);
    const partial = await ex.export([record("a"), record("b")], { gzip: false });
    expect(partial).toMatchObject({ accepted: 0, rejected: 2 });
    const denied = await ex.export([record("c")], { gzip: false });
    expect(denied.error).toBeInstanceOf(AuthenticationError);
    expect(denied.error?.message).toBe("logs.export: bad token (gRPC UNAUTHENTICATED)");
    await ex.shutdown();
  });
});

describe("createLogClient / toReadableLogRecord", () => {
  it("builds a BatchingLogClient with the config resource and batch settings", async () => {
    const s = await collector(() => ({ body: OTLP_OK }));
    const cfg = new OtelCollectorConfig({
      url: s.url,
      serviceName: "svc-a",
      serviceVersion: "1.2",
      environment: "staging",
      resourceAttributes: { team: "core" },
      logsBatch: { maxBatchSize: 2 },
    });
    const client = createLogClient(cfg);
    expect(client.resource).toEqual({
      team: "core",
      "service.name": "svc-a",
      "service.version": "1.2",
      "deployment.environment.name": "staging",
    });
    client.logger("app").info("first", { k: "v" });
    client.send({ message: "second", severity: Severity.Error });
    client.send({ message: "third" });
    await client.close();
    expect(client.stats()).toMatchObject({ sent: 3, dropped: 0, failed: 0 });
    expect(s.requests.length).toBe(2);
    const own = createLogClient(
      { url: s.url },
      { resource: { x: "y" }, batch: { maxBatchSize: 1 } },
    );
    expect(own.resource).toEqual({ x: "y" });
    await own.close();
  });

  it("converts timestamps, severity and span context", () => {
    const resource = resourceFromAttributes({ "service.name": "svc" });
    const r = toReadableLogRecord(
      record("b", { traceId: "4bf92f3577b34da6a3ce929d0e0e4736", traceFlags: 1 }),
      resource,
    );
    expect(r.hrTime).toEqual([Date.parse("2026-11-18T10:00:00Z") / 1000, 123_000_000]);
    expect(r.hrTimeObserved).toEqual(r.hrTime);
    expect(r.severityNumber).toBe(13);
    expect(r.spanContext).toEqual({
      traceId: "4bf92f3577b34da6a3ce929d0e0e4736",
      spanId: "0000000000000000",
      traceFlags: 1,
    });
    const observed = toReadableLogRecord(
      record("c", { observedTimestamp: new Date(1000) }),
      resource,
    );
    expect(observed.hrTimeObserved).toEqual([1, 0]);
    expect(observed.spanContext).toBeUndefined();
  });
});
