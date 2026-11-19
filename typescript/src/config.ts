// Service-owned: configuration of the OpenTelemetry Collector client and of every OTLP exporter
// this package builds (traces, logs, metrics).

import { basename } from "node:path";
import {
  type ClientLogger,
  type CommonConfig,
  type CommonConfigOptions,
  ConfigError,
  type EnvMap,
  EnvReader,
  type Hooks,
  REDACTED,
  type RetryPolicy,
  type TlsOptions,
  describeCommonConfig,
  isSensitiveHeader,
  mergeOptions,
  parseDuration,
  readCommonEnv,
  redactUrl,
  resolveCommonConfig,
  validateCommonConfig,
} from "./core/index.js";
import type { BatchOptions } from "./logging/index.js";
import type { SamplerName, SpanAttributes, TracingBatchConfig } from "./tracing/index.js";

const INSPECT = Symbol.for("nodejs.util.inspect.custom");

/** OTLP transport. `http/protobuf` (port 4318) is the default; `grpc` uses port 4317. */
export type OtlpProtocol = "http/protobuf" | "grpc";

/** The three OpenTelemetry signals. */
export type Signal = "traces" | "logs" | "metrics";

/** Resource detectors run by `initTelemetry` (see {@link detectResourceAttributes}). */
export type ResourceDetectorName = "host" | "process" | "container" | "k8s";

/** Every signal, in a fixed order. */
export const SIGNALS: readonly Signal[] = Object.freeze(["traces", "logs", "metrics"]);

/** Every resource detector, in a fixed order. */
export const RESOURCE_DETECTORS: readonly ResourceDetectorName[] = Object.freeze([
  "host",
  "process",
  "container",
  "k8s",
]);

const PROTOCOLS: readonly string[] = ["http/protobuf", "grpc"];
const SAMPLERS: readonly string[] = [
  "always_on",
  "always_off",
  "traceidratio",
  "parentbased_always_on",
  "parentbased_always_off",
  "parentbased_traceidratio",
];

/** Options accepted by {@link OtelCollectorConfig} (all optional). */
export interface OtelCollectorConfigOptions extends CommonConfigOptions {
  /**
   * OTLP/HTTP base URL (port 4318); signal paths `/v1/traces`, `/v1/logs`, `/v1/metrics` are
   * appended. Default `http://127.0.0.1:4318`.
   */
  url?: string;
  /**
   * OTLP/gRPC target (port 4317). Default: scheme and host of `url`, port 4317
   * (`http://127.0.0.1:4317`); `https://` enables TLS.
   */
  grpcUrl?: string;
  /** Export protocol. Default `http/protobuf`. `native` is rejected with `ConfigError`. */
  protocol?: OtlpProtocol;
  /**
   * Full traces endpoint, used as-is: overrides `url` + `/v1/traces` for HTTP and `grpcUrl` for gRPC.
   */
  tracesUrl?: string;
  /**
   * Full logs endpoint, used as-is: overrides `url` + `/v1/logs` for HTTP and `grpcUrl` for gRPC.
   */
  logsUrl?: string;
  /**
   * Full metrics endpoint, used as-is: overrides `url` + `/v1/metrics` for HTTP and `grpcUrl` for gRPC.
   */
  metricsUrl?: string;
  /** Extra headers for trace exports only (merged over `headers`). Redacted. */
  tracesHeaders?: Record<string, string>;
  /** Extra headers for log exports only (merged over `headers`). Redacted. */
  logsHeaders?: Record<string, string>;
  /** Extra headers for metric exports only (merged over `headers`). Redacted. */
  metricsHeaders?: Record<string, string>;
  /** Payload compression of OTLP exports. Default `gzip`. */
  compression?: "gzip" | "none";
  /** Deadline of one OTLP export including the exporter's own retries, in ms. Default 10 000. */
  exportTimeoutMs?: number;
  /** `health_check` extension URL. Default: host of `url`, port 13133, path `/`. */
  healthUrl?: string;
  /** zpages extension base URL. Default: host of `url`, port 55679. */
  zpagesUrl?: string;
  /** Internal telemetry (Prometheus) URL such as `http://collector:8888/metrics`. Optional. */
  telemetryUrl?: string;
  /** `service.name`. Default `unknown_service:<process>`. */
  serviceName?: string;
  /** `service.version`. */
  serviceVersion?: string;
  /** `deployment.environment.name`. */
  environment?: string;
  /** Resource attributes; explicit keys win over environment keys. */
  resourceAttributes?: SpanAttributes;
  /** Export traces. Default true. */
  traces?: boolean;
  /** Export logs. Default true. */
  logs?: boolean;
  /** Export metrics. Default true. */
  metrics?: boolean;
  /** Trace sampler. Default `parentbased_always_on`. */
  sampler?: SamplerName;
  /** Ratio of the `*traceidratio` samplers (0..1). Default 1. */
  samplerRatio?: number;
  /** Propagators. Default `["tracecontext", "baggage"]`. */
  propagators?: readonly string[];
  /** Batch span processor settings. */
  tracesBatch?: TracingBatchConfig;
  /** Log batch processor settings. */
  logsBatch?: BatchOptions;
  /** Interval of the periodic metric reader in ms. Default 60 000. */
  metricExportIntervalMs?: number;
  /** Report Node.js runtime metrics (memory, CPU, event loop, uptime). Default false. */
  runtimeMetrics?: boolean;
  /** Resource detectors run by `initTelemetry`. Default: all ({@link RESOURCE_DETECTORS}). */
  resourceDetectors?: readonly ResourceDetectorName[];
}

/** Standard OpenTelemetry variables consulted after `GROW_OPENTELEMETRY_COLLECTOR_*`/`GROW_OTEL_*`. */
export const VENDOR_ENV = Object.freeze({
  endpoint: "OTEL_EXPORTER_OTLP_ENDPOINT",
  protocol: "OTEL_EXPORTER_OTLP_PROTOCOL",
  headers: "OTEL_EXPORTER_OTLP_HEADERS",
  compression: "OTEL_EXPORTER_OTLP_COMPRESSION",
  timeout: "OTEL_EXPORTER_OTLP_TIMEOUT",
  certificate: "OTEL_EXPORTER_OTLP_CERTIFICATE",
  clientCertificate: "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE",
  clientKey: "OTEL_EXPORTER_OTLP_CLIENT_KEY",
  serviceName: "OTEL_SERVICE_NAME",
  resourceAttributes: "OTEL_RESOURCE_ATTRIBUTES",
  sdkDisabled: "OTEL_SDK_DISABLED",
  sampler: "OTEL_TRACES_SAMPLER",
  samplerArg: "OTEL_TRACES_SAMPLER_ARG",
  propagators: "OTEL_PROPAGATORS",
  metricExportInterval: "OTEL_METRIC_EXPORT_INTERVAL",
});

/** Alias prefix accepted below `GROW_OPENTELEMETRY_COLLECTOR_` (ticket wording). */
export const ALIAS_ENV_PREFIX = "GROW_OTEL_";

function isHttpUrl(value: string): boolean {
  try {
    const u = new URL(value);
    return u.protocol === "http:" || u.protocol === "https:";
  } catch {
    return false;
  }
}

/** URL with the same scheme and host as `base` and another port/path. */
function sibling(base: string, port: number, path: string): string {
  const u = new URL(base);
  return `${u.protocol}//${u.hostname}:${port}${path}`;
}

function defaultServiceName(): string {
  const name = basename(process.argv0 === "" ? "node" : process.argv0);
  return `unknown_service:${name}`;
}

/**
 * Parses `k=v,k2=v2` (W3C baggage style, values percent-decoded) as used by
 * `OTEL_EXPORTER_OTLP_HEADERS` and `OTEL_RESOURCE_ATTRIBUTES`. Throws `ConfigError` naming
 * `source` (never the value) on malformed input.
 */
export function parseKeyValueList(raw: string, source: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const part of raw.split(",")) {
    if (part.trim() === "") continue;
    const eq = part.indexOf("=");
    if (eq <= 0 || part.slice(0, eq).trim() === "")
      throw new ConfigError(
        `invalid value in environment variable ${source}: expected key=value pairs`,
      );
    try {
      out[decodeURIComponent(part.slice(0, eq).trim())] = decodeURIComponent(
        part.slice(eq + 1).trim(),
      );
    } catch {
      throw new ConfigError(
        `invalid value in environment variable ${source}: bad percent-encoding`,
      );
    }
  }
  return out;
}

type Layer = OtelCollectorConfigOptions;

function upper(signal: Signal): string {
  return signal.toUpperCase();
}

function invalid(name: string, expected: string): ConfigError {
  return new ConfigError(`invalid value in environment variable ${name}: expected ${expected}`);
}

/** Reads one `GROW_*` layer (primary prefix or the `GROW_OTEL_` alias). */
function readGrowLayer(reader: EnvReader): Layer {
  const layer: Layer = readCommonEnv(reader);
  const str = (name: string): string | undefined => reader.string(name);
  const set = <K extends keyof Layer>(key: K, value: Layer[K] | undefined): void => {
    if (value !== undefined) layer[key] = value;
  };
  set("grpcUrl", str("GRPC_URL"));
  const protocol = reader.lookup("PROTOCOL");
  if (protocol !== undefined) layer.protocol = parseProtocol(protocol.value, protocol.name);
  for (const signal of SIGNALS) set(`${signal}Url`, str(`${upper(signal)}_URL`));
  const headers = reader.lookup("HEADERS");
  if (headers !== undefined) layer.headers = parseKeyValueList(headers.value, headers.name);
  const compression = reader.lookup("COMPRESSION");
  if (compression !== undefined)
    layer.compression = parseCompression(compression.value, compression.name);
  set("exportTimeoutMs", reader.duration("EXPORT_TIMEOUT"));
  set("healthUrl", str("HEALTH_URL"));
  set("zpagesUrl", str("ZPAGES_URL"));
  set("telemetryUrl", str("TELEMETRY_URL"));
  set("serviceName", str("SERVICE_NAME"));
  set("serviceVersion", str("SERVICE_VERSION"));
  set("environment", str("ENVIRONMENT"));
  const attrs = reader.lookup("RESOURCE_ATTRIBUTES");
  if (attrs !== undefined) layer.resourceAttributes = parseKeyValueList(attrs.value, attrs.name);
  for (const signal of SIGNALS) set(signal, reader.bool(upper(signal)));
  const sampler = reader.lookup("SAMPLER");
  if (sampler !== undefined) layer.sampler = parseSampler(sampler.value, sampler.name);
  const ratio = reader.lookup("SAMPLER_RATIO");
  if (ratio !== undefined) layer.samplerRatio = parseRatio(ratio.value, ratio.name);
  const propagators = str("PROPAGATORS");
  if (propagators !== undefined) layer.propagators = parseList(propagators);
  set("metricExportIntervalMs", reader.duration("METRIC_EXPORT_INTERVAL"));
  set("runtimeMetrics", reader.bool("RUNTIME_METRICS"));
  const detectors = reader.lookup("RESOURCE_DETECTORS");
  if (detectors !== undefined)
    layer.resourceDetectors = parseDetectors(detectors.value, detectors.name);
  return layer;
}

function parseProtocol(value: string, name: string): OtlpProtocol {
  const v = value.trim().toLowerCase();
  if (!PROTOCOLS.includes(v)) throw invalid(name, "grpc or http/protobuf");
  return v as OtlpProtocol;
}

function parseCompression(value: string, name: string): "gzip" | "none" {
  const v = value.trim().toLowerCase();
  if (v !== "gzip" && v !== "none") throw invalid(name, "gzip or none");
  return v;
}

function parseSampler(value: string, name: string): SamplerName {
  const v = value.trim().toLowerCase();
  if (!SAMPLERS.includes(v)) throw invalid(name, "a known sampler name");
  return v as SamplerName;
}

function parseRatio(value: string, name: string): number {
  const n = Number(value.trim());
  if (value.trim() === "" || !Number.isFinite(n) || n < 0 || n > 1)
    throw invalid(name, "a ratio between 0 and 1");
  return n;
}

function parseList(value: string): string[] {
  return value
    .split(",")
    .map((p) => p.trim())
    .filter((p) => p !== "");
}

function parseDetectors(value: string, name: string): ResourceDetectorName[] {
  const list = parseList(value.toLowerCase());
  if (list.length === 1 && list[0] === "none") return [];
  for (const d of list)
    if (!(RESOURCE_DETECTORS as readonly string[]).includes(d))
      throw invalid(name, "host, process, container, k8s or none");
  return list as ResourceDetectorName[];
}

function positiveInt(env: EnvMap, name: string): number | undefined {
  const raw = env[name]?.trim();
  if (raw === undefined || raw === "") return undefined;
  if (!/^\d+$/.test(raw) || Number(raw) <= 0) throw invalid(name, "a positive integer");
  return Number(raw);
}

function msValue(env: EnvMap, name: string): number | undefined {
  const raw = env[name]?.trim();
  if (raw === undefined || raw === "") return undefined;
  const ms = parseDuration(raw);
  if (ms === undefined || ms <= 0) throw invalid(name, "a duration in milliseconds");
  return ms;
}

/**
 * Reads the standard `OTEL_*` layer. `protocol` is the effective protocol (decided by the
 * higher layers first) because `OTEL_EXPORTER_OTLP_ENDPOINT` feeds `url` for HTTP and `grpcUrl`
 * for gRPC.
 */
function readOtelLayer(env: EnvMap, higherProtocol: OtlpProtocol | undefined): Layer {
  const hit = (name: string): string | undefined => {
    const v = env[name];
    return v === undefined || v.trim() === "" ? undefined : v.trim();
  };
  const layer: Layer = {};
  const protocolRaw = hit(VENDOR_ENV.protocol);
  if (protocolRaw !== undefined) layer.protocol = parseProtocol(protocolRaw, VENDOR_ENV.protocol);
  const protocol = higherProtocol ?? layer.protocol ?? "http/protobuf";
  const endpoint = hit(VENDOR_ENV.endpoint);
  if (endpoint !== undefined) {
    if (protocol === "grpc") layer.grpcUrl = endpoint;
    else layer.url = endpoint;
  }
  for (const signal of SIGNALS) {
    const signalEndpoint = hit(`OTEL_EXPORTER_OTLP_${upper(signal)}_ENDPOINT`);
    if (signalEndpoint !== undefined) layer[`${signal}Url`] = signalEndpoint;
    const signalHeaders = hit(`OTEL_EXPORTER_OTLP_${upper(signal)}_HEADERS`);
    if (signalHeaders !== undefined)
      layer[`${signal}Headers`] = parseKeyValueList(
        signalHeaders,
        `OTEL_EXPORTER_OTLP_${upper(signal)}_HEADERS`,
      );
  }
  const headers = hit(VENDOR_ENV.headers);
  if (headers !== undefined) layer.headers = parseKeyValueList(headers, VENDOR_ENV.headers);
  const compression = hit(VENDOR_ENV.compression);
  if (compression !== undefined)
    layer.compression = parseCompression(compression, VENDOR_ENV.compression);
  const timeout = msValue(env, VENDOR_ENV.timeout);
  if (timeout !== undefined) layer.exportTimeoutMs = timeout;
  const tls: TlsOptions = {};
  const ca = hit(VENDOR_ENV.certificate);
  if (ca !== undefined) tls.caCertPath = ca;
  const cert = hit(VENDOR_ENV.clientCertificate);
  if (cert !== undefined) tls.clientCertPath = cert;
  const key = hit(VENDOR_ENV.clientKey);
  if (key !== undefined) tls.clientKeyPath = key;
  if (Object.keys(tls).length > 0) layer.tls = tls;

  const resourceRaw = hit(VENDOR_ENV.resourceAttributes);
  const resource =
    resourceRaw === undefined ? {} : parseKeyValueList(resourceRaw, VENDOR_ENV.resourceAttributes);
  const { "service.name": resourceServiceName, ...rest } = resource;
  if (resourceRaw !== undefined) layer.resourceAttributes = rest;
  const serviceName = hit(VENDOR_ENV.serviceName) ?? resourceServiceName;
  if (serviceName !== undefined) layer.serviceName = serviceName;

  const disabled = hit(VENDOR_ENV.sdkDisabled);
  if (disabled !== undefined && disabled.toLowerCase() === "true") {
    layer.traces = false;
    layer.logs = false;
    layer.metrics = false;
  } else {
    for (const signal of SIGNALS) {
      const exporter = hit(`OTEL_${upper(signal)}_EXPORTER`);
      if (exporter !== undefined && exporter.toLowerCase() === "none") layer[signal] = false;
    }
  }
  const sampler = hit(VENDOR_ENV.sampler);
  if (sampler !== undefined) {
    layer.sampler = parseSampler(sampler, VENDOR_ENV.sampler);
    const arg = hit(VENDOR_ENV.samplerArg);
    if (arg !== undefined && layer.sampler.endsWith("traceidratio"))
      layer.samplerRatio = parseRatio(arg, VENDOR_ENV.samplerArg);
  }
  const propagators = hit(VENDOR_ENV.propagators);
  if (propagators !== undefined) layer.propagators = parseList(propagators);
  const interval = msValue(env, VENDOR_ENV.metricExportInterval);
  if (interval !== undefined) layer.metricExportIntervalMs = interval;

  const bsp: TracingBatchConfig = {};
  const bspDelay = msValue(env, "OTEL_BSP_SCHEDULE_DELAY");
  if (bspDelay !== undefined) bsp.scheduledDelayMs = bspDelay;
  const bspTimeout = msValue(env, "OTEL_BSP_EXPORT_TIMEOUT");
  if (bspTimeout !== undefined) bsp.exportTimeoutMs = bspTimeout;
  const bspQueue = positiveInt(env, "OTEL_BSP_MAX_QUEUE_SIZE");
  if (bspQueue !== undefined) bsp.maxQueueSize = bspQueue;
  const bspBatch = positiveInt(env, "OTEL_BSP_MAX_EXPORT_BATCH_SIZE");
  if (bspBatch !== undefined) bsp.maxExportBatchSize = bspBatch;
  if (Object.keys(bsp).length > 0) layer.tracesBatch = bsp;

  const blrp: BatchOptions = {};
  const blrpDelay = msValue(env, "OTEL_BLRP_SCHEDULE_DELAY");
  if (blrpDelay !== undefined) blrp.flushIntervalMs = blrpDelay;
  const blrpTimeout = msValue(env, "OTEL_BLRP_EXPORT_TIMEOUT");
  if (blrpTimeout !== undefined) blrp.exportTimeoutMs = blrpTimeout;
  const blrpQueue = positiveInt(env, "OTEL_BLRP_MAX_QUEUE_SIZE");
  if (blrpQueue !== undefined) blrp.maxQueueSize = blrpQueue;
  const blrpBatch = positiveInt(env, "OTEL_BLRP_MAX_EXPORT_BATCH_SIZE");
  if (blrpBatch !== undefined) blrp.maxBatchSize = blrpBatch;
  if (Object.keys(blrp).length > 0) layer.logsBatch = blrp;
  return layer;
}

const MERGED_MAPS = [
  "resourceAttributes",
  "tracesHeaders",
  "logsHeaders",
  "metricsHeaders",
  "tracesBatch",
  "logsBatch",
] as const;

/** {@link mergeOptions} plus key-by-key merging of the attribute, header and batch maps. */
function mergeLayers(...layers: Layer[]): Layer {
  const merged = mergeOptions<Layer>(...layers);
  for (const key of MERGED_MAPS) {
    const parts = layers.map((l) => l[key]).filter((v) => v !== undefined);
    if (parts.length > 0) (merged as Record<string, unknown>)[key] = Object.assign({}, ...parts);
  }
  return merged;
}

function frozenMap<T extends object>(value: T | undefined): Readonly<T> {
  return Object.freeze({ ...(value ?? ({} as T)) });
}

/** Immutable configuration of {@link OtelCollectorClient} and of the OTLP exporters. */
export class OtelCollectorConfig implements CommonConfig {
  /** Default OTLP/HTTP base URL. */
  static readonly DEFAULT_URL = "http://127.0.0.1:4318";
  /** Default OTLP/gRPC target. */
  static readonly DEFAULT_GRPC_URL = "http://127.0.0.1:4317";
  /** Default export deadline in ms. */
  static readonly DEFAULT_EXPORT_TIMEOUT_MS = 10_000;
  /** Default metric export interval in ms. */
  static readonly DEFAULT_METRIC_EXPORT_INTERVAL_MS = 60_000;
  /** Prefix of every environment variable. */
  static readonly ENV_PREFIX = "GROW_OPENTELEMETRY_COLLECTOR_";

  readonly url: string;
  readonly connectTimeoutMs: number;
  readonly requestTimeoutMs: number;
  readonly totalTimeoutMs: number;
  readonly retry: RetryPolicy;
  readonly tls: Readonly<TlsOptions>;
  readonly proxy?: string;
  readonly headers: Readonly<Record<string, string>>;
  readonly userAgentSuffix?: string;
  readonly hooks?: Hooks;
  readonly logger?: ClientLogger;
  /** OTLP/gRPC target. */
  readonly grpcUrl: string;
  /** Export protocol. */
  readonly protocol: OtlpProtocol;
  /** Explicit OTLP/HTTP traces URL. */
  readonly tracesUrl?: string;
  /** Explicit OTLP/HTTP logs URL. */
  readonly logsUrl?: string;
  /** Explicit OTLP/HTTP metrics URL. */
  readonly metricsUrl?: string;
  /** Trace-only export headers. */
  readonly tracesHeaders: Readonly<Record<string, string>>;
  /** Log-only export headers. */
  readonly logsHeaders: Readonly<Record<string, string>>;
  /** Metric-only export headers. */
  readonly metricsHeaders: Readonly<Record<string, string>>;
  /** Export compression. */
  readonly compression: "gzip" | "none";
  /** Export deadline in ms. */
  readonly exportTimeoutMs: number;
  /** `health_check` URL. */
  readonly healthUrl: string;
  /** Whether {@link healthUrl} was set explicitly. */
  readonly healthUrlExplicit: boolean;
  /** zpages base URL. */
  readonly zpagesUrl: string;
  /** Internal telemetry (Prometheus) URL. */
  readonly telemetryUrl?: string;
  /** `service.name`. */
  readonly serviceName: string;
  /** `service.version`. */
  readonly serviceVersion?: string;
  /** `deployment.environment.name`. */
  readonly environment?: string;
  /** Resource attributes. */
  readonly resourceAttributes: Readonly<SpanAttributes>;
  /** Trace export on/off. */
  readonly traces: boolean;
  /** Log export on/off. */
  readonly logs: boolean;
  /** Metric export on/off. */
  readonly metrics: boolean;
  /** Trace sampler. */
  readonly sampler: SamplerName;
  /** Sampler ratio. */
  readonly samplerRatio: number;
  /** Propagators. */
  readonly propagators: readonly string[];
  /** Batch span processor settings (unset fields use the SPEC defaults). */
  readonly tracesBatch: Readonly<TracingBatchConfig>;
  /** Log batch settings (unset fields use the SPEC defaults). */
  readonly logsBatch: Readonly<BatchOptions>;
  /** Metric export interval in ms. */
  readonly metricExportIntervalMs: number;
  /** Runtime metrics on/off. */
  readonly runtimeMetrics: boolean;
  /** Resource detectors run by `initTelemetry`. */
  readonly resourceDetectors: readonly ResourceDetectorName[];

  /** Applies defaults, validates (throws {@link ConfigError}) and freezes. */
  constructor(options: OtelCollectorConfigOptions = {}) {
    const common = resolveCommonConfig(options, OtelCollectorConfig.DEFAULT_URL);
    this.url = common.url;
    this.connectTimeoutMs = common.connectTimeoutMs;
    this.requestTimeoutMs = common.requestTimeoutMs;
    this.totalTimeoutMs = common.totalTimeoutMs;
    this.retry = common.retry;
    this.tls = common.tls;
    this.headers = common.headers;
    if (common.proxy !== undefined) this.proxy = common.proxy;
    if (common.userAgentSuffix !== undefined) this.userAgentSuffix = common.userAgentSuffix;
    if (common.hooks !== undefined) this.hooks = common.hooks;
    if (common.logger !== undefined) this.logger = common.logger;

    const protocol = (options.protocol as string | undefined) ?? "http/protobuf";
    if (protocol === "native")
      throw new ConfigError(
        "invalid configuration: the OpenTelemetry Collector has no native protocol; use grpc or http/protobuf",
      );
    if (!PROTOCOLS.includes(protocol))
      throw new ConfigError("invalid configuration: protocol must be grpc or http/protobuf");
    this.protocol = protocol as OtlpProtocol;
    if (options.tracesUrl !== undefined) this.tracesUrl = options.tracesUrl;
    if (options.logsUrl !== undefined) this.logsUrl = options.logsUrl;
    if (options.metricsUrl !== undefined) this.metricsUrl = options.metricsUrl;
    this.tracesHeaders = frozenMap(options.tracesHeaders);
    this.logsHeaders = frozenMap(options.logsHeaders);
    this.metricsHeaders = frozenMap(options.metricsHeaders);
    this.compression = options.compression ?? "gzip";
    this.exportTimeoutMs = options.exportTimeoutMs ?? OtelCollectorConfig.DEFAULT_EXPORT_TIMEOUT_MS;
    const urlOk = isHttpUrl(this.url);
    this.grpcUrl =
      options.grpcUrl ??
      (urlOk ? sibling(this.url, 4317, "") : OtelCollectorConfig.DEFAULT_GRPC_URL);
    this.healthUrlExplicit = options.healthUrl !== undefined;
    this.healthUrl = options.healthUrl ?? (urlOk ? sibling(this.url, 13133, "/") : "");
    this.zpagesUrl = options.zpagesUrl ?? (urlOk ? sibling(this.url, 55679, "") : "");
    if (options.telemetryUrl !== undefined) this.telemetryUrl = options.telemetryUrl;
    const serviceName = options.serviceName?.trim();
    this.serviceName =
      serviceName === undefined || serviceName === "" ? defaultServiceName() : serviceName;
    if (options.serviceVersion !== undefined) this.serviceVersion = options.serviceVersion;
    if (options.environment !== undefined) this.environment = options.environment;
    this.resourceAttributes = frozenMap(options.resourceAttributes);
    this.traces = options.traces ?? true;
    this.logs = options.logs ?? true;
    this.metrics = options.metrics ?? true;
    this.sampler = options.sampler ?? "parentbased_always_on";
    this.samplerRatio = options.samplerRatio ?? 1;
    this.propagators = Object.freeze([...(options.propagators ?? ["tracecontext", "baggage"])]);
    this.tracesBatch = frozenMap(options.tracesBatch);
    this.logsBatch = frozenMap(options.logsBatch);
    this.metricExportIntervalMs =
      options.metricExportIntervalMs ?? OtelCollectorConfig.DEFAULT_METRIC_EXPORT_INTERVAL_MS;
    this.runtimeMetrics = options.runtimeMetrics ?? false;
    this.resourceDetectors = Object.freeze([...(options.resourceDetectors ?? RESOURCE_DETECTORS)]);
    this.validate();
    Object.freeze(this);
  }

  /** Returns `options` when it already is a config, otherwise builds one. */
  static from(options: OtelCollectorConfigOptions = {}): OtelCollectorConfig {
    if (options instanceof OtelCollectorConfig) return options;
    return new OtelCollectorConfig(options);
  }

  /**
   * Builds a config from the environment. Precedence per field: explicit `overrides` >
   * `GROW_OPENTELEMETRY_COLLECTOR_<NAME>` > `GROW_OTEL_<NAME>` > per-signal `OTEL_*` > generic
   * `OTEL_*` > default. Map fields (`headers`, `resourceAttributes`, per-signal headers, batch
   * settings) merge key by key in the same order. A present but invalid value throws
   * `ConfigError` naming the variable, never its value.
   */
  static fromEnv(
    overrides: OtelCollectorConfigOptions = {},
    env: EnvMap = process.env,
  ): OtelCollectorConfig {
    const primary = readGrowLayer(new EnvReader(OtelCollectorConfig.ENV_PREFIX, env));
    const alias = readGrowLayer(new EnvReader(ALIAS_ENV_PREFIX, env));
    const higherProtocol = overrides.protocol ?? primary.protocol ?? alias.protocol;
    const otel = readOtelLayer(env, higherProtocol);
    return new OtelCollectorConfig(mergeLayers(otel, alias, primary, overrides));
  }

  /** OTLP/HTTP URL of a signal: the explicit `<signal>Url`, else `url` + `/v1/<signal>`. */
  signalUrl(signal: Signal): string {
    const explicit = this[`${signal}Url`];
    if (explicit !== undefined) return explicit;
    return `${this.url.replace(/\/+$/, "")}/v1/${signal}`;
  }

  /**
   * The export endpoint of a signal for the configured protocol: the explicit `<signal>Url`
   * when set (in both protocols), else `grpcUrl` for gRPC and `url` + `/v1/<signal>` for HTTP.
   */
  endpointFor(signal: Signal): string {
    if (this.protocol !== "grpc") return this.signalUrl(signal);
    return this[`${signal}Url`] ?? this.grpcUrl;
  }

  /** Export headers of a signal: `headers` with the per-signal headers on top. */
  headersFor(signal: Signal): Record<string, string> {
    return { ...this.headers, ...this[`${signal}Headers`] };
  }

  /** Throws {@link ConfigError} when a setting is invalid. */
  validate(): void {
    validateCommonConfig(this);
    const problems: string[] = [];
    for (const key of [
      "grpcUrl",
      "healthUrl",
      "zpagesUrl",
      "tracesUrl",
      "logsUrl",
      "metricsUrl",
      "telemetryUrl",
    ] as const) {
      const value = this[key];
      if (value !== undefined && !isHttpUrl(value))
        problems.push(`${key} must be an http or https URL`);
    }
    if (!["gzip", "none"].includes(this.compression))
      problems.push("compression must be gzip or none");
    for (const key of ["exportTimeoutMs", "metricExportIntervalMs"] as const) {
      if (!Number.isFinite(this[key]) || this[key] <= 0)
        problems.push(`${key} must be a positive number`);
    }
    if (!SAMPLERS.includes(this.sampler)) problems.push("sampler is unknown");
    if (!Number.isFinite(this.samplerRatio) || this.samplerRatio < 0 || this.samplerRatio > 1)
      problems.push("samplerRatio must be between 0 and 1");
    for (const d of this.resourceDetectors)
      if (!(RESOURCE_DETECTORS as readonly string[]).includes(d))
        problems.push(`resource detector "${d}" is unknown`);
    if (problems.length > 0) throw new ConfigError(`invalid configuration: ${problems.join("; ")}`);
  }

  /** Values of sensitive export headers (`authorization`, `*token*`, `*key*`, …) to redact. */
  knownSecrets(): string[] {
    const out: string[] = [];
    for (const map of [this.headers, this.tracesHeaders, this.logsHeaders, this.metricsHeaders])
      for (const [k, v] of Object.entries(map)) if (isSensitiveHeader(k)) out.push(v);
    return out;
  }

  /** Debug form: every header value redacted, URL credentials hidden. */
  toJSON(): Record<string, unknown> {
    const redactAll = (m: Readonly<Record<string, string>>): Record<string, string> =>
      Object.fromEntries(Object.keys(m).map((k) => [k, REDACTED]));
    const out: Record<string, unknown> = {
      ...describeCommonConfig(this),
      grpcUrl: redactUrl(this.grpcUrl),
      protocol: this.protocol,
      tracesHeaders: redactAll(this.tracesHeaders),
      logsHeaders: redactAll(this.logsHeaders),
      metricsHeaders: redactAll(this.metricsHeaders),
      compression: this.compression,
      exportTimeoutMs: this.exportTimeoutMs,
      healthUrl: redactUrl(this.healthUrl),
      zpagesUrl: redactUrl(this.zpagesUrl),
      serviceName: this.serviceName,
      resourceAttributes: this.resourceAttributes,
      traces: this.traces,
      logs: this.logs,
      metrics: this.metrics,
      sampler: this.sampler,
      samplerRatio: this.samplerRatio,
      propagators: this.propagators,
      metricExportIntervalMs: this.metricExportIntervalMs,
      runtimeMetrics: this.runtimeMetrics,
      resourceDetectors: this.resourceDetectors,
    };
    for (const key of ["tracesUrl", "logsUrl", "metricsUrl", "telemetryUrl"] as const) {
      const v = this[key];
      if (v !== undefined) out[key] = redactUrl(v);
    }
    if (this.serviceVersion !== undefined) out.serviceVersion = this.serviceVersion;
    if (this.environment !== undefined) out.environment = this.environment;
    return out;
  }

  toString(): string {
    return `OtelCollectorConfig(${JSON.stringify(this.toJSON())})`;
  }

  [INSPECT](): string {
    return this.toString();
  }
}
