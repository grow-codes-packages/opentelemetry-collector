// Service-owned: OTLP log export — `OtlpLogExporter` (SPEC §4 `LogExporter`) built on the official
// OpenTelemetry OTLP export delegates (http/protobuf or gRPC, OTLP retry rules), plus the
// `createLogClient` helper that wraps it in the template `BatchingLogClient`.

import type { HrTime } from "@opentelemetry/api";
import type { SeverityNumber } from "@opentelemetry/api-logs";
import { ExportResultCode, type ExportResult as OtelExportResult } from "@opentelemetry/core";
import { OTLPExporterBase } from "@opentelemetry/otlp-exporter-base";
import {
  convertLegacyHttpOptions,
  createOtlpHttpExportDelegate,
} from "@opentelemetry/otlp-exporter-base/node-http";
import {
  convertLegacyOtlpGrpcOptions,
  createOtlpGrpcExportDelegate,
} from "@opentelemetry/otlp-grpc-exporter-base";
import {
  LogsExporterMetricsHelper,
  ProtobufLogsSerializer,
  type IExportLogsServiceResponse,
  type ISerializer,
} from "@opentelemetry/otlp-transformer";
import { resourceFromAttributes, type Resource } from "@opentelemetry/resources";
import type { ReadableLogRecord } from "@opentelemetry/sdk-logs";
import { CancelledError, ValidationError, SDK_VERSION } from "./core/index.js";
import { OtelCollectorConfig, type OtelCollectorConfigOptions } from "./config.js";
import {
  BatchingLogClient,
  type Attributes,
  type BatchingLogClientOptions,
  type ExportOptions,
  type ExportResult,
  type LogExporter,
  type LogRecord,
} from "./logging/index.js";
import {
  compressionAlgorithm,
  GRPC_EXPORT_PATHS,
  grpcChannelCredentials,
  grpcMetadata,
  httpAgentOptions,
  loadTls,
  mapExportError,
  USER_AGENT,
} from "./otlp.js";

/** Instrumentation scope of records exported by {@link OtlpLogExporter}. */
export const LOG_SCOPE = Object.freeze({
  name: "@grow-codes/opentelemetry-collector",
  version: SDK_VERSION,
});

type Delegate = OTLPExporterBase<ReadableLogRecord[]>;

/** Partial-success capture: the delegate deserializes the response right before its callback. */
interface Capture {
  rejected: number;
  message?: string;
}

function toHrTime(date: Date): HrTime {
  const ms = date.getTime();
  const seconds = Math.floor(ms / 1000);
  return [seconds, Math.round((ms - seconds * 1000) * 1e6)];
}

/**
 * Converts an SPEC §4 {@link LogRecord} to the OpenTelemetry SDK record the OTLP serializers
 * take. `resource` must be the shared Resource object for `record.resource`.
 */
export function toReadableLogRecord(record: LogRecord, resource: Resource): ReadableLogRecord {
  const out: {
    -readonly [K in keyof ReadableLogRecord]: ReadableLogRecord[K];
  } = {
    hrTime: toHrTime(record.timestamp),
    hrTimeObserved: toHrTime(record.observedTimestamp ?? record.timestamp),
    severityNumber: record.severity as unknown as SeverityNumber,
    severityText: record.severityText,
    body: record.body,
    resource,
    instrumentationScope: LOG_SCOPE,
    attributes: record.attributes,
    droppedAttributesCount: 0,
  };
  if (record.traceId !== undefined)
    out.spanContext = {
      traceId: record.traceId,
      spanId: record.spanId ?? "0000000000000000",
      traceFlags: record.traceFlags ?? 0,
    };
  return out;
}

/**
 * {@link LogExporter} that sends records to the collector over OTLP: `POST {logsUrl}`
 * (`application/x-protobuf`) or gRPC `LogsService/Export`. Retries (HTTP 429/502/503/504) are
 * done by the official OTLP export delegate within `exportTimeoutMs`; errors in the result keep
 * the natural `retryable` flag of their code. A partial success reports `rejected` and a
 * `ValidationError`.
 */
export class OtlpLogExporter implements LogExporter {
  /** The configuration in use. */
  readonly config: OtelCollectorConfig;
  /** Endpoint records are sent to (`logsUrl` when set, else `url` + `/v1/logs` for HTTP or `grpcUrl` for gRPC). */
  readonly endpoint: string;
  readonly #delegates = new Map<boolean, Delegate>();
  readonly #resources = new Map<string, Resource>();
  #capture: Capture | undefined;
  #shutdown = false;

  /** Accepts a {@link OtelCollectorConfig} or plain options. */
  constructor(config: OtelCollectorConfigOptions = {}) {
    this.config = OtelCollectorConfig.from(config);
    this.endpoint = this.config.endpointFor("logs");
  }

  #serializer(): ISerializer<ReadableLogRecord[], IExportLogsServiceResponse> {
    return {
      serializeRequest: (records) => ProtobufLogsSerializer.serializeRequest(records),
      deserializeResponse: (data) => {
        const response = ProtobufLogsSerializer.deserializeResponse(data);
        const partial = response.partialSuccess;
        // protobufjs may decode int64 as a Long-like object; Number() handles both.
        const raw: unknown = partial?.rejectedLogRecords ?? 0;
        const rejected = Number(raw);
        this.#capture = { rejected: Number.isFinite(rejected) ? rejected : 0 };
        if (partial?.errorMessage) this.#capture.message = partial.errorMessage;
        return response;
      },
    };
  }

  #delegate(gzip: boolean): Delegate {
    const existing = this.#delegates.get(gzip);
    if (existing !== undefined) return existing;
    const cfg = this.config;
    const tls = loadTls(cfg.tls);
    const compression = compressionAlgorithm(gzip ? cfg.compression : "none");
    const headers = cfg.headersFor("logs");
    let delegate: Delegate;
    if (cfg.protocol === "grpc") {
      delegate = new OTLPExporterBase(
        createOtlpGrpcExportDelegate(
          // The official OTLP exporters build their options with the same helper.
          // eslint-disable-next-line @typescript-eslint/no-deprecated
          convertLegacyOtlpGrpcOptions(
            {
              url: this.endpoint,
              metadata: grpcMetadata(headers),
              credentials: grpcChannelCredentials(this.endpoint, tls),
              compression,
              timeoutMillis: cfg.exportTimeoutMs,
              userAgent: USER_AGENT,
            },
            "LOGS",
          ),
          this.#serializer(),
          "otlp_grpc_log_exporter",
          LogsExporterMetricsHelper,
          undefined,
          "LogsExportService",
          GRPC_EXPORT_PATHS.logs,
        ),
      );
    } else {
      const agent = httpAgentOptions(tls);
      delegate = new OTLPExporterBase(
        createOtlpHttpExportDelegate(
          // eslint-disable-next-line @typescript-eslint/no-deprecated
          convertLegacyHttpOptions(
            {
              url: this.endpoint,
              headers,
              compression,
              timeoutMillis: cfg.exportTimeoutMs,
              userAgent: USER_AGENT,
              ...(agent === undefined ? {} : { httpAgentOptions: agent }),
            },
            "LOGS",
            "v1/logs",
            { "Content-Type": "application/x-protobuf" },
          ),
          this.#serializer(),
          "otlp_http_log_exporter",
          LogsExporterMetricsHelper,
          undefined,
        ),
      );
    }
    this.#delegates.set(gzip, delegate);
    return delegate;
  }

  #resource(attributes: Attributes): Resource {
    const key = JSON.stringify(attributes);
    let resource = this.#resources.get(key);
    if (resource === undefined) {
      if (this.#resources.size >= 64) this.#resources.clear();
      resource = resourceFromAttributes(attributes as Record<string, never>);
      this.#resources.set(key, resource);
    }
    return resource;
  }

  /**
   * Exports `records` in one OTLP request. Resolves with `accepted`/`rejected` counts; never
   * rejects (failures are reported in `error`).
   */
  async export(records: LogRecord[], opts: ExportOptions = { gzip: true }): Promise<ExportResult> {
    const n = records.length;
    if (n === 0) return { accepted: 0, rejected: 0 };
    const operation = "logs.export";
    if (this.#shutdown)
      return {
        accepted: 0,
        rejected: n,
        error: new ValidationError(`${operation}: exporter is shut down`, { retryable: false }),
      };
    if (opts.signal?.aborted === true)
      return { accepted: 0, rejected: n, error: new CancelledError(`${operation}: cancelled`) };
    let delegate: Delegate;
    try {
      delegate = this.#delegate(opts.gzip);
    } catch (err) {
      return { accepted: 0, rejected: n, error: mapExportError(operation, err) };
    }
    const readable = records.map((r) => toReadableLogRecord(r, this.#resource(r.resource)));
    const secrets = this.config.knownSecrets();
    const result = new Promise<ExportResult>((resolve) => {
      delegate.export(readable, (res: OtelExportResult) => {
        const capture = this.#capture;
        this.#capture = undefined;
        if (res.code !== ExportResultCode.SUCCESS) {
          resolve({
            accepted: 0,
            rejected: n,
            error: mapExportError(operation, res.error, secrets),
          });
          return;
        }
        const rejected = Math.min(n, capture?.rejected ?? 0);
        if (rejected === 0) {
          resolve({ accepted: n, rejected: 0 });
          return;
        }
        resolve({
          accepted: n - rejected,
          rejected,
          error: new ValidationError(
            `${operation}: the collector rejected ${rejected} of ${n} records${
              capture?.message === undefined ? "" : ` (${capture.message.slice(0, 200)})`
            }`,
            { retryable: false },
          ),
        });
      });
    });
    const signal = opts.signal;
    if (signal === undefined) return result;
    let onAbort: (() => void) | undefined;
    const aborted = new Promise<ExportResult>((resolve) => {
      onAbort = () => {
        resolve({ accepted: 0, rejected: n, error: new CancelledError(`${operation}: cancelled`) });
      };
      signal.addEventListener("abort", onAbort, { once: true });
    });
    try {
      return await Promise.race([result, aborted]);
    } finally {
      signal.removeEventListener("abort", onAbort as () => void);
    }
  }

  /** Waits for in-flight exports and releases connections. Idempotent. */
  async shutdown(): Promise<void> {
    if (this.#shutdown) return;
    this.#shutdown = true;
    const delegates = [...this.#delegates.values()];
    this.#delegates.clear();
    await Promise.all(delegates.map((d) => d.shutdown().catch(() => undefined)));
  }
}

/** Options of {@link createLogClient}. */
export type CreateLogClientOptions = BatchingLogClientOptions;

/**
 * A {@link BatchingLogClient} exporting OTLP logs to the collector. `resource` defaults to
 * `service.name` (+ version and environment) and the configured resource attributes; `batch`
 * defaults to the configured `logsBatch` (`OTEL_BLRP_*`).
 */
export function createLogClient(
  config: OtelCollectorConfigOptions = {},
  options: CreateLogClientOptions = {},
): BatchingLogClient {
  const cfg = OtelCollectorConfig.from(config);
  const resource: Attributes = {
    ...(cfg.resourceAttributes as Attributes),
    "service.name": cfg.serviceName,
  };
  if (cfg.serviceVersion !== undefined) resource["service.version"] = cfg.serviceVersion;
  if (cfg.environment !== undefined) resource["deployment.environment.name"] = cfg.environment;
  return new BatchingLogClient(new OtlpLogExporter(cfg), {
    ...options,
    resource: options.resource ?? resource,
    batch: { ...cfg.logsBatch, ...options.batch },
  });
}
