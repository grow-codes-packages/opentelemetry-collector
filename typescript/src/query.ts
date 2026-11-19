// Service-owned: the collector stores nothing and has no query API, so the SPEC §4/§5 query
// clients throw `UnsupportedError` from every method.

import { UnsupportedError } from "./core/index.js";
import { UnsupportedLogQueryClient } from "./logging/index.js";
import type { DependencyLink, Trace, TraceQueryClient, TraceSummary } from "./tracing/index.js";

function unsupported(op: string): Promise<never> {
  return Promise.reject(
    new UnsupportedError(
      `tracing.${op}: the OpenTelemetry Collector has no query API (read traces from the backend it exports to)`,
    ),
  );
}

/** {@link TraceQueryClient} of the collector: every method rejects with `UnsupportedError`. */
export class CollectorTraceQueryClient implements TraceQueryClient {
  /** Always `UnsupportedError`. */
  getTrace(_traceId: string): Promise<Trace | null> {
    return unsupported("getTrace");
  }

  /** Always `UnsupportedError`. */
  searchTraces(_query?: unknown): Promise<TraceSummary[]> {
    return unsupported("searchTraces");
  }

  /** Always `UnsupportedError`. */
  listServices(): Promise<string[]> {
    return unsupported("listServices");
  }

  /** Always `UnsupportedError`. */
  listOperations(_service: string): Promise<string[]> {
    return unsupported("listOperations");
  }

  /** Always `UnsupportedError`. */
  getDependencies(_opts?: unknown): Promise<DependencyLink[]> {
    return unsupported("getDependencies");
  }

  /** Always `UnsupportedError`. */
  waitForTrace(_traceId: string, _opts?: unknown): Promise<Trace> {
    return unsupported("waitForTrace");
  }
}

/** {@link LogQueryClient} of the collector: every method throws `UnsupportedError`. */
export class CollectorLogQueryClient extends UnsupportedLogQueryClient {}
