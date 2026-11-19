import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import {
  CancelledError,
  ConfigError,
  ErrorCode,
  NotFoundError,
  codeForGrpcStatus,
  decodeRpcStatus,
  errorForGrpcStatus,
  grpcChannelCredentials,
  grpcMetadata,
  grpcTarget,
  httpAgentOptions,
  loadTls,
  mapExportError,
  otlpErrorText,
  refSleep,
  CollectorTraceQueryClient,
  CollectorLogQueryClient,
  UnsupportedError,
  type LogQueryClient,
} from "../../src/index.js";
import { fixture } from "./mock.js";

describe("OTLP error bodies", () => {
  it("decodes the recorded google.rpc.Status (401 fixture)", () => {
    const body = Buffer.from(
      fixture("ping.http_unauthenticated_missing_token").response.body?.base64 as string,
      "base64",
    );
    expect(decodeRpcStatus(body)).toEqual({
      code: 16,
      message: "missing or empty authorization header: Authorization",
    });
    expect(otlpErrorText(body)).toBe("missing or empty authorization header: Authorization");
    expect(otlpErrorText(body.toString("utf8"))).toBe(
      "missing or empty authorization header: Authorization",
    );
  });

  it("handles JSON, plain text, empty, malformed and long bodies", () => {
    expect(otlpErrorText('{"code":3,"message":"ReadObject: bad"}')).toBe("ReadObject: bad");
    expect(otlpErrorText('{"code":3}')).toBeUndefined();
    expect(otlpErrorText("404 page not found\n")).toBe("404 page not found");
    expect(otlpErrorText("")).toBeUndefined();
    expect(otlpErrorText(undefined)).toBeUndefined();
    expect(otlpErrorText("   ")).toBeUndefined();
    expect(otlpErrorText("x".repeat(400))?.length).toBe(300);
    expect(otlpErrorText("token=abcdef1234 leaked", [])).not.toContain("abcdef1234");
    expect(otlpErrorText("my sekret-value here", ["sekret-value"])).not.toContain("sekret-value");
    expect(decodeRpcStatus(Uint8Array.from([0x0b]))).toBeUndefined();
    expect(decodeRpcStatus(Uint8Array.from([0x08]))).toBeUndefined();
    expect(decodeRpcStatus(Uint8Array.from([0x12, 0x05, 0x61]))).toBeUndefined();
    expect(decodeRpcStatus(Uint8Array.from([0x80]))).toBeUndefined();
    expect(decodeRpcStatus(Uint8Array.from([0x18, 0x01, 0x22, 0x01, 0x61]))).toEqual({});
    expect(otlpErrorText(Uint8Array.from([0x0b, 0x41, 0x42]))).toBe("AB");
  });
});

describe("gRPC status mapping", () => {
  it("maps every status code", () => {
    const expected: Record<number, string> = {
      1: "CANCELLED",
      2: "UNAVAILABLE",
      3: "VALIDATION",
      4: "TIMEOUT",
      5: "NOT_FOUND",
      6: "CONFLICT",
      7: "PERMISSION_DENIED",
      8: "RATE_LIMITED",
      9: "VALIDATION",
      10: "UNAVAILABLE",
      11: "VALIDATION",
      12: "NOT_FOUND",
      13: "VALIDATION",
      14: "UNAVAILABLE",
      15: "UNAVAILABLE",
      16: "AUTHENTICATION",
    };
    for (const [code, name] of Object.entries(expected))
      expect(codeForGrpcStatus(Number(code)), code).toBe(name);
    const err = errorForGrpcStatus("op", 12, "unknown method Nope");
    expect(err).toBeInstanceOf(NotFoundError);
    expect(err.message).toBe("op: unknown method Nope (gRPC UNIMPLEMENTED)");
    expect(errorForGrpcStatus("op", 99, undefined).message).toBe(
      "op: service unavailable (gRPC 99)",
    );
    expect(errorForGrpcStatus("op", 14, "", [], false).retryable).toBe(false);
  });
});

describe("mapExportError", () => {
  it("maps HTTP, gRPC, timeout and generic exporter errors (natural retryable flag)", () => {
    const http = mapExportError("x", { code: 403, data: '{"message":"denied"}' });
    expect(http.code).toBe(ErrorCode.PermissionDenied);
    expect(http.message).toBe("x: denied (HTTP 403)");
    expect(http.retryable).toBe(false);
    expect(mapExportError("x", { code: 500 }).message).toBe("x: service unavailable (HTTP 500)");
    expect(mapExportError("x", { code: 503 }).retryable).toBe(true);
    expect(mapExportError("x", { code: 14, details: "down" }).retryable).toBe(true);
    const grpc = mapExportError("x", { code: 7, details: "nope" });
    expect(grpc.code).toBe(ErrorCode.PermissionDenied);
    expect(grpc.retryable).toBe(false);
    expect(mapExportError("x", new Error("Request timed out")).code).toBe(ErrorCode.Timeout);
    const generic = mapExportError("x", new Error("Bearer abc.def leaked"));
    expect(generic.code).toBe(ErrorCode.Unavailable);
    expect(generic.retryable).toBe(true);
    expect(generic.message).not.toContain("abc.def");
    expect(mapExportError("x", undefined).code).toBe(ErrorCode.Unavailable);
    expect(mapExportError("x", "plain").message).toBe("x: plain");
    const own = new ConfigError("c");
    expect(mapExportError("x", own)).toBe(own);
  });
});

describe("exporter options", () => {
  it("loads TLS files and builds agent options and credentials", () => {
    const dir = mkdtempSync(join(tmpdir(), "otel-tls-"));
    try {
      const ca = join(dir, "ca.pem");
      writeFileSync(ca, "CA");
      writeFileSync(join(dir, "c.pem"), "CERT");
      writeFileSync(join(dir, "k.pem"), "KEY");
      const tls = loadTls({
        caCertPath: ca,
        caCertPem: "PEM",
        clientCertPath: join(dir, "c.pem"),
        clientKeyPath: join(dir, "k.pem"),
        insecureSkipVerify: true,
      });
      expect(tls.ca?.toString()).toBe("PEMCA");
      expect(httpAgentOptions(tls)).toMatchObject({ rejectUnauthorized: false });
      expect(Object.keys(httpAgentOptions(tls) ?? {})).toEqual([
        "ca",
        "cert",
        "key",
        "rejectUnauthorized",
      ]);
      expect(loadTls({ caCertPem: "P" }).ca?.toString()).toBe("P");
      expect(loadTls({ caCertPath: ca }).ca?.toString()).toBe("CA");
      expect(httpAgentOptions(loadTls({}))).toBeUndefined();
      expect(() => loadTls({ caCertPath: join(dir, "missing") })).toThrow(ConfigError);
      expect(grpcChannelCredentials("http://h:4317", loadTls({}))._isSecure()).toBe(false);
      expect(grpcChannelCredentials("https://h:4317", loadTls({}))._isSecure()).toBe(true);
      expect(
        grpcChannelCredentials("http://h:4317", loadTls({ insecureSkipVerify: true }))._isSecure(),
      ).toBe(true);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("builds gRPC targets and metadata", () => {
    expect(grpcTarget("http://c:4317")).toBe("c:4317");
    expect(grpcTarget("http://c")).toBe("c:80");
    expect(grpcTarget("https://c")).toBe("c:443");
    expect(grpcTarget("http://[::1]:4317")).toBe("[::1]:4317");
    expect(grpcMetadata({ "X-A": "1" }).get("x-a")).toEqual(["1"]);
  });

  it("refSleep resolves and honours abort", async () => {
    await refSleep(1);
    await expect(refSleep(10, AbortSignal.abort())).rejects.toBeInstanceOf(CancelledError);
    const c = new AbortController();
    const p = refSleep(5000, c.signal);
    c.abort();
    await expect(p).rejects.toBeInstanceOf(CancelledError);
    await refSleep(-5, new AbortController().signal);
  });
});

describe("query clients", () => {
  it("every method throws UnsupportedError", async () => {
    const t = new CollectorTraceQueryClient();
    for (const p of [
      t.getTrace("a"),
      t.searchTraces({}),
      t.listServices(),
      t.listOperations("s"),
      t.getDependencies({}),
      t.waitForTrace("a"),
    ])
      await expect(p).rejects.toBeInstanceOf(UnsupportedError);
    const l: LogQueryClient = new CollectorLogQueryClient();
    await expect(l.search("x")).rejects.toBeInstanceOf(UnsupportedError);
    expect(() => l.tail("x", () => undefined)).toThrow(UnsupportedError);
    await expect(l.waitForLog("x")).rejects.toBeInstanceOf(UnsupportedError);
  });
});
