import { fileURLToPath } from "node:url";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { OtelCollectorClient } from "../../src/client.js";
import {
  type ConformanceOptions,
  conformanceCases,
  formatReport,
  runConformance,
} from "../../src/testing/index.js";
import { type MockCollector, OTLP_OK, fixtureResponse, startCollector } from "../collector/mock.js";

const scenariosDir = fileURLToPath(new URL("../../../conformance/scenarios/", import.meta.url));
let server: MockCollector;

beforeAll(async () => {
  server = await startCollector((req) => {
    if (req.url === "/") return fixtureResponse("health.ok");
    if (req.url === "/debug/servicez") return fixtureResponse("serverVersion.zpages");
    if (req.url === "/v1/traces") return { body: OTLP_OK };
    return { status: 404 };
  });
});

afterAll(async () => {
  await server.close();
});

function options(): ConformanceOptions {
  return {
    scenariosDir,
    categories: ["core"],
    factories: {
      core: () =>
        new OtelCollectorClient({
          url: server.url,
          healthUrl: `${server.url}/`,
          zpagesUrl: server.url,
          headers: { authorization: "Bearer conformance-token" },
        }),
    },
    capabilities: { core: ["version"] },
  };
}

describe("conformance (core) against a mock collector (recorded fixtures)", () => {
  it("every core scenario passes", async () => {
    const report = await runConformance(options());
    expect(report.failed, formatReport(report)).toBe(0);
    expect(report.skipped).toBe(0);
    expect(report.results.map((r) => r.id)).toContain("core.health");
  });

  for (const c of conformanceCases(options())) {
    it.skipIf(c.skip !== undefined)(`case ${c.id}`, async () => {
      expect((await c.run()).status).toBe("passed");
    });
  }
});
