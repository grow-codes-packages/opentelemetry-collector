// Integration tests against the real collector (docker/compose.yml): client surface, health
// (incl. the stock collector's loopback-only health_check), ping over HTTP and gRPC (with the
// bearertokenauth receiver), serverVersion, the OTLP log exporter and the query clients.

import assert from "node:assert/strict";
import { before, test } from "node:test";
import {
  AuthenticationError,
  CollectorHealthClient,
  OtelCollectorClient,
  OtelCollectorConfig,
  OtelCollectorError,
  OtlpLogExporter,
  Severity,
  UnsupportedError,
} from "@grow-codes/opentelemetry-collector";
import { loadBootstrapEnv, runId } from "./env.ts";
import { capturedLogs, eventually } from "./readback.ts";

function env(name: string): string {
  const v = process.env[name];
  assert.ok(v, `${name} must be set (from /bootstrap/env)`);
  return v;
}

before(() => {
  loadBootstrapEnv();
  env("GROW_OPENTELEMETRY_COLLECTOR_URL");
});

test("fromEnv reads the bootstrap variables", async () => {
  const client = OtelCollectorClient.fromEnv();
  try {
    assert.equal(client.config.url, env("GROW_OPENTELEMETRY_COLLECTOR_URL"));
    assert.equal(client.config.grpcUrl, env("GROW_OPENTELEMETRY_COLLECTOR_GRPC_URL"));
    assert.equal(client.config.protocol, "http/protobuf");
    assert.ok(client.config instanceof OtelCollectorConfig);
  } finally {
    await client.close();
  }
});

test("health reports the health_check extension; waitUntilReady returns it", async () => {
  const client = OtelCollectorClient.fromEnv();
  try {
    const health = await client.health();
    assert.equal(health.healthy, true, JSON.stringify(health));
    assert.equal(health.status, "ok");
    assert.equal(health.details.source, "health_check");
    assert.equal(health.details.vendorStatus, "Server available");
    assert.match(health.details.upSince ?? "", /^\d{4}-\d\d-\d\dT/);
    const ready = await client.waitUntilReady(10_000);
    assert.equal(ready.healthy, true);
  } finally {
    await client.close();
  }
});

test("stock collector: health_check is loopback-only, health falls back to zpages", async () => {
  const stockUrl = env("OTELCOL_TEST_STOCK_URL");
  const host = new URL(stockUrl).hostname;
  const health = new CollectorHealthClient({ url: stockUrl, retry: { maxRetries: 0 } });
  try {
    const h = await health.health();
    assert.deepEqual(h, { healthy: true, status: "ok", details: { source: "zpages" } });
    assert.equal(await health.serverVersion(), env("OTELCOL_TEST_SERVER_VERSION"));
    assert.equal((await health.waitUntilReady(5000)).healthy, true);
  } finally {
    await health.close();
  }
  const unreachable = new CollectorHealthClient({
    url: stockUrl,
    zpagesUrl: `http://${host}:1`,
    retry: { maxRetries: 0 },
  });
  try {
    const h = await unreachable.health();
    assert.equal(h.healthy, false);
    assert.equal(h.status, "unreachable");
  } finally {
    await unreachable.close();
  }
});

test("ping succeeds over HTTP and gRPC", async () => {
  for (const protocol of ["http/protobuf", "grpc"] as const) {
    const client = OtelCollectorClient.fromEnv({ protocol });
    try {
      const latency = await client.ping();
      assert.equal(typeof latency, "number");
      assert.ok(latency >= 0);
    } finally {
      await client.close();
    }
  }
});

test("ping against the bearertokenauth receiver: token accepted, wrong token AUTHENTICATION", async () => {
  const token = env("OTELCOL_TEST_BEARER_TOKEN");
  const wrong = "wrong-token-for-integration";
  const targets = [
    { protocol: "http/protobuf" as const, url: env("OTELCOL_TEST_AUTH_URL") },
    { protocol: "grpc" as const, grpcUrl: env("OTELCOL_TEST_AUTH_GRPC_URL") },
  ];
  for (const target of targets) {
    const ok = new OtelCollectorClient({
      ...target,
      headers: { authorization: `Bearer ${token}` },
    });
    try {
      assert.ok((await ok.ping()) >= 0);
    } finally {
      await ok.close();
    }
    const bad = new OtelCollectorClient({
      ...target,
      headers: { authorization: `Bearer ${wrong}` },
    });
    try {
      await assert.rejects(bad.ping(), (err: unknown) => {
        assert.ok(err instanceof AuthenticationError, String(err));
        assert.equal(err.code, "AUTHENTICATION");
        assert.ok(!err.message.includes(wrong));
        assert.ok(!err.message.includes(token));
        return true;
      });
    } finally {
      await bad.close();
    }
  }
});

test("serverVersion reads zpages, falls back to target_info", async () => {
  const want = env("OTELCOL_TEST_SERVER_VERSION");
  const client = OtelCollectorClient.fromEnv();
  try {
    assert.equal(await client.serverVersion(), want);
  } finally {
    await client.close();
    await client.close();
  }
  const host = new URL(env("GROW_OPENTELEMETRY_COLLECTOR_URL")).hostname;
  const fallback = new OtelCollectorClient({
    url: env("GROW_OPENTELEMETRY_COLLECTOR_URL"),
    zpagesUrl: `http://${host}:1777`,
    telemetryUrl: env("GROW_OPENTELEMETRY_COLLECTOR_TELEMETRY_URL"),
  });
  try {
    assert.equal(await fallback.serverVersion(), want);
  } finally {
    await fallback.close();
  }
});

test("OtlpLogExporter round trip over HTTP and gRPC, auth failure mapped", async () => {
  for (const protocol of ["http/protobuf", "grpc"] as const) {
    const marker = `exporter-${protocol.replace("/", "-")}-${runId}-${Date.now()}`;
    const client = OtelCollectorClient.fromEnv({ protocol });
    const exporter = client.logExporter();
    try {
      const result = await exporter.export(
        [
          {
            timestamp: new Date(),
            severity: Severity.Error,
            severityText: "ERROR",
            body: marker,
            attributes: { run: runId, n: 7 },
            resource: { "service.name": "it-exporter" },
            traceId: "4bf92f3577b34da6a3ce929d0e0e4736",
            spanId: "00f067aa0ba902b7",
          },
        ],
        { gzip: true },
      );
      assert.deepEqual(result, { accepted: 1, rejected: 0 });
      const found = await eventually(async () =>
        (await capturedLogs()).find((l) => l.body === marker),
      );
      assert.equal(found.resource["service.name"], "it-exporter");
      assert.equal(found.attributes.n, 7);
      assert.equal(found.record.severityNumber, 17);
      assert.equal(found.record.severityText, "ERROR");
      assert.equal(found.record.traceId, "4bf92f3577b34da6a3ce929d0e0e4736");
      assert.equal(found.record.spanId, "00f067aa0ba902b7");
    } finally {
      await client.close();
    }
  }
  const denied = new OtlpLogExporter({ url: env("OTELCOL_TEST_AUTH_URL") });
  const result = await denied.export(
    [
      {
        timestamp: new Date(),
        severity: Severity.Info,
        severityText: "INFO",
        body: "denied",
        attributes: {},
        resource: {},
      },
    ],
    { gzip: false },
  );
  assert.equal(result.accepted, 0);
  assert.ok(result.error instanceof OtelCollectorError);
  assert.equal(result.error.code, "AUTHENTICATION");
  await denied.shutdown();
});

test("query clients are unsupported (the collector stores nothing)", async () => {
  const client = OtelCollectorClient.fromEnv();
  try {
    await assert.rejects(
      client.traceQuery().getTrace("4bf92f3577b34da6a3ce929d0e0e4736"),
      UnsupportedError,
    );
    await assert.rejects(client.traceQuery().listServices(), UnsupportedError);
    await assert.rejects(client.logQuery().search("x"), UnsupportedError);
  } finally {
    await client.close();
  }
});
