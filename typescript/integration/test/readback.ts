// Test-only read-back for the collector fixture (the collector itself has no query API):
// - the file exporter output served by `capture` (OTLP JSON lines),
// - the Jaeger sidecar (`/api/v3/traces/{id}`, `/api/v3/services`).
// The conformance query adapters live here, never in the shipped package (DECISIONS.md §7).

import {
  BaseLogQueryClient,
  BaseTraceQueryClient,
  type LogRecord,
  type LogSearchOptions,
  type LogSearchResult,
  type Span,
  type Trace,
  type TraceSummary,
} from "@grow-codes/opentelemetry-collector";

type Json = Record<string, unknown>;
type AttrValue = string | number | boolean | AttrValue[] | { [k: string]: AttrValue };

export function captureUrl(): string {
  const url = process.env.OTELCOL_TEST_CAPTURE_URL;
  if (url === undefined) throw new Error("OTELCOL_TEST_CAPTURE_URL is not set");
  return url;
}

export function jaegerUrl(): string {
  const url = process.env.OTELCOL_TEST_JAEGER_URL;
  if (url === undefined) throw new Error("OTELCOL_TEST_JAEGER_URL is not set");
  return url;
}

/** OTLP JSON AnyValue → plain value. */
export function anyValue(v: Json | undefined): AttrValue {
  if (v === undefined) return "";
  if ("stringValue" in v) return v.stringValue as string;
  if ("intValue" in v) return Number(v.intValue);
  if ("doubleValue" in v) return Number(v.doubleValue);
  if ("boolValue" in v) return v.boolValue as boolean;
  if ("arrayValue" in v)
    return (((v.arrayValue as Json).values as Json[] | undefined) ?? []).map(anyValue);
  if ("kvlistValue" in v) return attrs((v.kvlistValue as Json).values as Json[] | undefined);
  return "";
}

/** OTLP JSON KeyValue list → map. */
export function attrs(list: Json[] | undefined): Record<string, AttrValue> {
  const out: Record<string, AttrValue> = {};
  for (const kv of list ?? []) out[kv.key as string] = anyValue(kv.value as Json | undefined);
  return out;
}

/** Every JSON line of `capture/<signal>.jsonl`. */
export async function captureLines(signal: "traces" | "logs" | "metrics"): Promise<Json[]> {
  const res = await fetch(`${captureUrl()}/${signal}.jsonl`);
  if (res.status === 404) return [];
  if (!res.ok) throw new Error(`capture ${signal}: HTTP ${res.status}`);
  return (await res.text())
    .split("\n")
    .filter((l) => l.trim() !== "")
    .map((l) => JSON.parse(l) as Json);
}

/** A captured log record with its resource. */
export interface CapturedLog {
  resource: Record<string, AttrValue>;
  scope: string;
  record: Json;
  body: string;
  attributes: Record<string, AttrValue>;
}

export async function capturedLogs(): Promise<CapturedLog[]> {
  const out: CapturedLog[] = [];
  for (const line of await captureLines("logs"))
    for (const rl of (line.resourceLogs as Json[] | undefined) ?? []) {
      const resource = attrs((rl.resource as Json | undefined)?.attributes as Json[] | undefined);
      for (const sl of (rl.scopeLogs as Json[] | undefined) ?? [])
        for (const record of (sl.logRecords as Json[] | undefined) ?? [])
          out.push({
            resource,
            scope: ((sl.scope as Json | undefined)?.name as string | undefined) ?? "",
            record,
            body: String(anyValue(record.body as Json | undefined)),
            attributes: attrs(record.attributes as Json[] | undefined),
          });
    }
  return out;
}

/** A captured metric with its resource. */
export interface CapturedMetric {
  resource: Record<string, AttrValue>;
  metric: Json;
}

export async function capturedMetrics(): Promise<CapturedMetric[]> {
  const out: CapturedMetric[] = [];
  for (const line of await captureLines("metrics"))
    for (const rm of (line.resourceMetrics as Json[] | undefined) ?? []) {
      const resource = attrs((rm.resource as Json | undefined)?.attributes as Json[] | undefined);
      for (const sm of (rm.scopeMetrics as Json[] | undefined) ?? [])
        for (const metric of (sm.metrics as Json[] | undefined) ?? [])
          out.push({ resource, metric });
    }
  return out;
}

/** Data points of a metric whose points carry `attr=value`. */
export function pointsWith(metric: Json, attr: string, value: string): Json[] {
  const body = (metric.sum ?? metric.gauge ?? metric.histogram) as Json | undefined;
  return ((body?.dataPoints as Json[] | undefined) ?? []).filter(
    (p) => attrs(p.attributes as Json[] | undefined)[attr] === value,
  );
}

/** Polls `fn` until it returns a non-undefined value (default 20 s). */
export async function eventually<T>(
  fn: () => Promise<T | undefined>,
  timeoutMs = 20_000,
): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const v = await fn();
    if (v !== undefined) return v;
    if (Date.now() > deadline) throw new Error("eventually: timed out");
    await new Promise((r) => setTimeout(r, 250));
  }
}

const KINDS: Span["kind"][] = [
  "unspecified",
  "internal",
  "server",
  "client",
  "producer",
  "consumer",
];
const STATUS: Span["status"]["code"][] = ["unset", "ok", "error"];

function micros(nanos: unknown): number {
  return Math.floor(Number(BigInt(String(nanos ?? "0")) / 1000n));
}

/** OTLP JSON `resourceSpans` → normalized {@link Trace}. */
export function traceFromOtlp(traceId: string, resourceSpans: Json[]): Trace {
  const spans: Span[] = [];
  for (const rs of resourceSpans) {
    const resource = attrs((rs.resource as Json | undefined)?.attributes as Json[] | undefined);
    for (const ss of (rs.scopeSpans as Json[] | undefined) ?? [])
      for (const s of (ss.spans as Json[] | undefined) ?? []) {
        const start = micros(s.startTimeUnixNano);
        const end = micros(s.endTimeUnixNano);
        const status = (s.status as Json | undefined) ?? {};
        const span: Span = {
          traceId: s.traceId as string,
          spanId: s.spanId as string,
          name: s.name as string,
          kind: KINDS[Number(s.kind ?? 0)] ?? "unspecified",
          serviceName: String(resource["service.name"] ?? ""),
          startMicros: start,
          endMicros: end,
          durationMicros: end - start,
          status: {
            code: STATUS[Number(status.code ?? 0)] ?? "unset",
            ...(typeof status.message === "string" ? { message: status.message } : {}),
          },
          attributes: attrs(s.attributes as Json[] | undefined),
          events: ((s.events as Json[] | undefined) ?? []).map((e) => ({
            name: e.name as string,
            timeMicros: micros(e.timeUnixNano),
            attributes: attrs(e.attributes as Json[] | undefined),
          })),
          links: ((s.links as Json[] | undefined) ?? []).map((l) => ({
            traceId: l.traceId as string,
            spanId: l.spanId as string,
            attributes: attrs(l.attributes as Json[] | undefined),
          })),
        };
        if (typeof s.parentSpanId === "string" && s.parentSpanId !== "")
          span.parentSpanId = s.parentSpanId;
        spans.push(span);
      }
  }
  spans.sort((a, b) => a.startMicros - b.startMicros || a.spanId.localeCompare(b.spanId));
  return { traceId, spans };
}

/** Jaeger v3 read-back used as the conformance `TraceQueryClient` (test only). */
export class JaegerReadback extends BaseTraceQueryClient {
  async getTrace(traceId: string): Promise<Trace | null> {
    const res = await fetch(`${jaegerUrl()}/api/v3/traces/${traceId}`);
    if (res.status === 404) return null;
    if (!res.ok) throw new Error(`jaeger getTrace: HTTP ${res.status}`);
    const body = (await res.json()) as { result?: { resourceSpans?: Json[] } };
    return traceFromOtlp(traceId, body.result?.resourceSpans ?? []);
  }

  searchTraces(): Promise<TraceSummary[]> {
    return Promise.resolve([]);
  }

  async listServices(): Promise<string[]> {
    const res = await fetch(`${jaegerUrl()}/api/v3/services`);
    const body = (await res.json()) as { services?: string[] };
    return body.services ?? [];
  }

  listOperations(): Promise<string[]> {
    return Promise.resolve([]);
  }
}

function toLogRecord(c: CapturedLog): LogRecord {
  const r = c.record;
  const out: LogRecord = {
    timestamp: new Date(micros(r.timeUnixNano) / 1000),
    severity: Number(r.severityNumber ?? 9),
    severityText: String(r.severityText ?? ""),
    body: c.body,
    attributes: c.attributes,
    resource: c.resource,
  };
  if (typeof r.traceId === "string" && r.traceId !== "") out.traceId = r.traceId;
  if (typeof r.spanId === "string" && r.spanId !== "") out.spanId = r.spanId;
  return out;
}

/** Capture-file read-back used as the conformance `LogQueryClient` (substring match on the body). */
export class CaptureLogReadback extends BaseLogQueryClient {
  async search(query: string, _opts?: LogSearchOptions): Promise<LogSearchResult> {
    const records = (await capturedLogs()).filter((c) => c.body.includes(query)).map(toLogRecord);
    return { records, total: records.length };
  }
}
