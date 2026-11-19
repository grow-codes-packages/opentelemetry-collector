// Service-owned: one-call telemetry bootstrap — traces, logs and metrics exported over OTLP to the
// collector with one shared resource (detected host/process/container/Kubernetes attributes).

import {
  metrics,
  ProxyTracerProvider,
  createNoopMeter,
  type Meter,
  type Tracer,
} from "@opentelemetry/api";
import { defaultResource, resourceFromAttributes, type Resource } from "@opentelemetry/resources";
import type { MeterProvider, PushMetricExporter } from "@opentelemetry/sdk-metrics";
import type { NodeTracerProvider } from "@opentelemetry/sdk-trace-node";
import { TimeoutError, type EnvMap } from "./core/index.js";
import { OtelCollectorConfig, type OtelCollectorConfigOptions } from "./config.js";
import {
  Logger,
  type Attributes,
  type BatchOptions,
  type LogClient,
  type RedactionOptions,
  type RedactionProcessor,
  type Severity,
} from "./logging/index.js";
import { createLogClient } from "./logs.js";
import {
  createMeterProvider,
  metricsApi,
  registerRuntimeMetrics,
  RUNTIME_METER_NAME,
  type MetricsApi,
  type RuntimeMetricsHandle,
} from "./metrics.js";
import { detectResourceAttributes, type DetectResourceOptions } from "./resource.js";
import { initTracing } from "./setup.js";
import type {
  InitTracingOptions,
  SpanAttrValue,
  SpanAttributes,
  TracingConfig,
  TracingHandle,
} from "./tracing/index.js";

/** Trace settings of {@link initTelemetry} (unset fields come from the collector config). */
export interface TelemetryTracesOptions extends Partial<
  Omit<TracingConfig, "serviceName" | "setGlobal" | "resourceAttributes">
> {
  /** Extra span processors, propagator factories and id generator (template options). */
  init?: Omit<InitTracingOptions, "defaults">;
}

/** Log settings of {@link initTelemetry}. */
export interface TelemetryLogsOptions {
  /** Batching (defaults: config `logsBatch`, then SPEC §4). */
  batch?: BatchOptions;
  /** Redaction before export; `false` disables it. */
  redaction?: RedactionProcessor | RedactionOptions | false;
  /** Minimum severity of `logger()` loggers. Default `Info`. */
  minSeverity?: Severity;
}

/** Metric settings of {@link initTelemetry}. */
export interface TelemetryMetricsOptions {
  /** Export interval in ms (default config `metricExportIntervalMs`). */
  exportIntervalMs?: number;
  /** Node.js runtime metrics (default config `runtimeMetrics`). */
  runtimeMetrics?: boolean;
  /** Replaces the OTLP exporter (tests). */
  exporter?: PushMetricExporter;
}

/** Options of {@link initTelemetry}. */
export interface InitTelemetryOptions {
  /** Collector config or options. Default `OtelCollectorConfig.fromEnv({}, env)`. */
  config?: OtelCollectorConfig | OtelCollectorConfigOptions;
  /** Environment for the default config; `false` ignores the environment. */
  env?: EnvMap | false;
  /** `false` disables traces; otherwise trace overrides. Default: config `traces`. */
  traces?: boolean | TelemetryTracesOptions;
  /** `false` disables logs; otherwise log options. Default: config `logs`. */
  logs?: boolean | TelemetryLogsOptions;
  /** `false` disables metrics; otherwise metric options. Default: config `metrics`. */
  metrics?: boolean | TelemetryMetricsOptions;
  /** Register the tracer and meter providers globally. Default true. */
  setGlobal?: boolean;
  /** Resource detection seams (detectors default to config `resourceDetectors`). */
  detect?: Omit<DetectResourceOptions, "detectors">;
}

/** A running telemetry setup (all three signals). */
export interface TelemetryHandle extends MetricsApi {
  /** The collector config in use. */
  readonly config: OtelCollectorConfig;
  /** Attributes of the shared resource (detected < env < explicit < service fields). */
  readonly resourceAttributes: Readonly<SpanAttributes>;
  /** The shared resource (with the SDK default attributes). */
  readonly resource: Resource;
  /** The tracing handle, when traces are enabled. */
  readonly tracing?: TracingHandle;
  /** The tracer provider, when traces are enabled. */
  readonly tracerProvider?: NodeTracerProvider;
  /** The OTLP log client, when logs are enabled. */
  readonly logClient?: LogClient;
  /** The meter provider, when metrics are enabled. */
  readonly meterProvider?: MeterProvider;
  /** A tracer (no-op when traces are disabled). */
  tracer(name?: string, version?: string): Tracer;
  /** A logger writing to {@link logClient} (drops records when logs are disabled). */
  logger(name?: string, attributes?: Attributes): Logger;
  /** Exports everything buffered in every enabled signal; `TimeoutError` after `timeoutMs`. */
  forceFlush(timeoutMs?: number): Promise<void>;
  /** Flushes and shuts every provider down; idempotent. Unregisters the globals it set. */
  shutdown(): Promise<void>;
}

function enabled<T extends object>(value: boolean | T | undefined, fallback: boolean): T | null {
  if (value === false) return null;
  if (value === undefined) return fallback ? ({} as T) : null;
  return value === true ? ({} as T) : value;
}

async function withTimeout(
  promise: Promise<unknown>,
  timeoutMs: number,
  op: string,
): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(() => {
      reject(new TimeoutError(`${op}: timed out after ${timeoutMs} ms`));
    }, timeoutMs);
    timer.unref();
  });
  try {
    await Promise.race([promise, timeout]);
  } finally {
    clearTimeout(timer);
  }
}

/** Resource attributes for a config: detected < config attributes < service fields. */
export function telemetryResourceAttributes(
  config: OtelCollectorConfig,
  detect: DetectResourceOptions = {},
): SpanAttributes {
  const attrs: Record<string, SpanAttrValue> = {
    ...detectResourceAttributes({
      ...detect,
      detectors: detect.detectors ?? config.resourceDetectors,
    }),
    ...config.resourceAttributes,
    "service.name": config.serviceName,
  };
  if (config.serviceVersion !== undefined) attrs["service.version"] = config.serviceVersion;
  if (config.environment !== undefined) {
    attrs["deployment.environment.name"] = config.environment;
    attrs["deployment.environment"] = config.environment;
  }
  return attrs;
}

const DROP_SINK = Object.freeze({ send(): void {} });

/**
 * One-call OpenTelemetry bootstrap for every Grow stack (the platform deploys the collector with
 * each stack): traces (`initTracing`), logs (`OtlpLogExporter` + `BatchingLogClient`) and metrics
 * (MeterProvider + OTLP exporter) with one shared resource. Signals follow the config
 * (`traces`/`logs`/`metrics`, `OTEL_SDK_DISABLED`, `OTEL_<SIGNAL>_EXPORTER=none`) unless the
 * options turn them on or off. Exporter failures never throw at the caller.
 */
export function initTelemetry(options: InitTelemetryOptions = {}): TelemetryHandle {
  const config =
    options.config !== undefined
      ? OtelCollectorConfig.from(options.config)
      : options.env === false
        ? new OtelCollectorConfig()
        : OtelCollectorConfig.fromEnv({}, options.env ?? process.env);
  const setGlobal = options.setGlobal ?? true;
  const resourceAttributes = Object.freeze(telemetryResourceAttributes(config, options.detect));
  const resource = defaultResource().merge(
    resourceFromAttributes(resourceAttributes as Record<string, never>),
  );

  const tracesOpts = enabled(options.traces, config.traces);
  const logsOpts = enabled(options.logs, config.logs);
  const metricsOpts = enabled(options.metrics, config.metrics);

  let tracing: TracingHandle | undefined;
  let logClient: LogClient | undefined;
  let meterProvider: MeterProvider | undefined;
  let runtime: RuntimeMetricsHandle | undefined;
  let registeredMeter = false;
  try {
    if (tracesOpts !== null) {
      const { init, ...tracingOverrides } = tracesOpts;
      tracing = initTracing(
        { ...tracingOverrides, resourceAttributes: { ...resourceAttributes }, setGlobal },
        { ...init, collector: config },
      );
    }
    if (logsOpts !== null) {
      logClient = createLogClient(config, {
        resource: resourceAttributes,
        ...(logsOpts.batch === undefined ? {} : { batch: logsOpts.batch }),
        ...(logsOpts.redaction === undefined ? {} : { redaction: logsOpts.redaction }),
        ...(logsOpts.minSeverity === undefined ? {} : { minSeverity: logsOpts.minSeverity }),
      });
    }
    if (metricsOpts !== null) {
      meterProvider = createMeterProvider(config, {
        resource,
        ...(metricsOpts.exportIntervalMs === undefined
          ? {}
          : { exportIntervalMs: metricsOpts.exportIntervalMs }),
        ...(metricsOpts.exporter === undefined ? {} : { exporter: metricsOpts.exporter }),
      });
      if (setGlobal) registeredMeter = metrics.setGlobalMeterProvider(meterProvider);
      if (metricsOpts.runtimeMetrics ?? config.runtimeMetrics)
        runtime = registerRuntimeMetrics(meterProvider.getMeter(RUNTIME_METER_NAME));
    }
  } catch (err) {
    void tracing?.shutdown();
    void logClient?.close();
    void meterProvider?.shutdown();
    throw err;
  }

  // A ProxyTracerProvider without a delegate hands out non-recording tracers.
  // eslint-disable-next-line @typescript-eslint/no-deprecated
  const noopTracers = new ProxyTracerProvider();
  const noopMeter: Meter = createNoopMeter();
  const getMeter = (name: string, version?: string): Meter =>
    meterProvider === undefined ? noopMeter : meterProvider.getMeter(name, version);
  let shutdownPromise: Promise<void> | null = null;

  const handle: TelemetryHandle = {
    config,
    resourceAttributes,
    resource,
    ...(tracing === undefined ? {} : { tracing, tracerProvider: tracing.tracerProvider }),
    ...(logClient === undefined ? {} : { logClient }),
    ...(meterProvider === undefined ? {} : { meterProvider }),
    ...metricsApi(getMeter),
    tracer(name?: string, version?: string): Tracer {
      const scope = name ?? config.serviceName;
      return tracing === undefined
        ? noopTracers.getTracer(scope, version)
        : tracing.tracer(scope, version);
    },
    logger(name?: string, attributes?: Attributes): Logger {
      if (logClient !== undefined) return logClient.logger(name, attributes);
      return new Logger(DROP_SINK, {
        ...(name === undefined ? {} : { name }),
        ...(attributes === undefined ? {} : { attributes }),
      });
    },
    async forceFlush(timeoutMs = 30_000): Promise<void> {
      await withTimeout(
        Promise.all([
          tracing?.forceFlush(timeoutMs),
          logClient?.flush(timeoutMs),
          meterProvider?.forceFlush(),
        ]),
        timeoutMs,
        "telemetry.forceFlush",
      );
    },
    shutdown(): Promise<void> {
      shutdownPromise ??= (async () => {
        runtime?.stop();
        const results = await Promise.allSettled([
          tracing?.shutdown(),
          logClient?.close(),
          meterProvider?.shutdown(),
        ]);
        if (registeredMeter) metrics.disable();
        const failed = results.find((r) => r.status === "rejected");
        if (failed?.status === "rejected") throw failed.reason;
      })();
      return shutdownPromise;
    },
  };
  return handle;
}
