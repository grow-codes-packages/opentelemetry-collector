// Runs the shared conformance scenarios (conformance/scenarios) against the real collector.
// The collector has no query API: the tracing and logging query clients are test-only read-back
// adapters (Jaeger sidecar, file-exporter capture) from readback.ts. Declared capabilities:
// core `version`; tracing `query`, `services`; logging `query` (no tail/labels/streams/dependencies).

import assert from "node:assert/strict";
import { before, test } from "node:test";
import {
  OtelCollectorClient,
  initTracing,
  createLogClient,
  OtelCollectorConfig,
} from "@grow-codes/opentelemetry-collector";
import { formatReport, runConformance } from "@grow-codes/opentelemetry-collector/testing";
import { loadBootstrapEnv, scenariosDir } from "./env.ts";
import { CaptureLogReadback, JaegerReadback } from "./readback.ts";

before(() => {
  loadBootstrapEnv();
});

test("conformance: core", async () => {
  const report = await runConformance({
    scenariosDir,
    categories: ["core"],
    factories: { core: () => OtelCollectorClient.fromEnv() },
    capabilities: { core: ["version"] },
  });
  console.log(formatReport(report));
  assert.equal(report.failed, 0, formatReport(report));
  assert.ok(report.passed > 0);
});

test("conformance: tracing (Jaeger read-back)", async () => {
  const report = await runConformance({
    scenariosDir,
    categories: ["tracing"],
    factories: {
      tracing: ({ run }) => ({
        handle: initTracing({
          serviceName: `conformance-otelcol-ts-${run}`,
          setGlobal: false,
          batch: { scheduledDelayMs: 200 },
        }),
        query: new JaegerReadback(),
      }),
    },
    capabilities: { tracing: ["query", "services"] },
  });
  console.log(formatReport(report));
  assert.equal(report.failed, 0, formatReport(report));
  assert.equal(report.skipped, 0, formatReport(report));
});

test("conformance: logging (file exporter read-back)", async () => {
  const report = await runConformance({
    scenariosDir,
    categories: ["logging"],
    factories: {
      logging: ({ batchOptions }) => ({
        client: createLogClient(
          OtelCollectorConfig.fromEnv({ serviceName: "conformance-otelcol-ts" }),
          {
            ...(batchOptions === undefined ? {} : { batch: batchOptions }),
          },
        ),
        query: new CaptureLogReadback(),
      }),
    },
    capabilities: { logging: ["query"] },
  });
  console.log(formatReport(report));
  assert.equal(report.failed, 0, formatReport(report));
  assert.equal(report.skipped, 0, formatReport(report));
});
