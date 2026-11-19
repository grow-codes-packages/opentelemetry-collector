// Service-owned: OTLP metrics — MeterProvider with the official OTLP metric exporter (periodic
// reader), `meter`/`counter`/`histogram`/`gauge` helpers and optional Node.js runtime metrics.

import { monitorEventLoopDelay, type IntervalHistogram } from "node:perf_hooks";
import {
  metrics,
  type BatchObservableCallback,
  type Counter,
  type Gauge,
  type Histogram,
  type Meter,
  type MetricOptions,
  type UpDownCounter,
} from "@opentelemetry/api";
import { OTLPMetricExporter as OtlpGrpcMetricExporter } from "@opentelemetry/exporter-metrics-otlp-grpc";
import { OTLPMetricExporter as OtlpProtoMetricExporter } from "@opentelemetry/exporter-metrics-otlp-proto";
import type { Resource } from "@opentelemetry/resources";
import {
  MeterProvider,
  PeriodicExportingMetricReader,
  type PushMetricExporter,
} from "@opentelemetry/sdk-metrics";
import type { OtelCollectorConfig } from "./config.js";
import { ConfigError } from "./core/index.js";
import {
  compressionAlgorithm,
  grpcChannelCredentials,
  grpcMetadata,
  httpAgentOptions,
  loadTls,
  USER_AGENT,
} from "./otlp.js";
import { DEFAULT_TRACER_NAME } from "./tracing/index.js";

/** Meter name used by the helpers when none is given. */
export const DEFAULT_METER_NAME = DEFAULT_TRACER_NAME;

/** Meter name of the runtime metrics. */
export const RUNTIME_METER_NAME = "@grow-codes/opentelemetry-collector/runtime";

/** Options of the instrument helpers. */
export interface InstrumentOptions {
  /** Instrument description. */
  description?: string;
  /** Unit (UCUM), e.g. `ms`, `By`, `{request}`. */
  unit?: string;
  /** Meter to create the instrument on. Default {@link DEFAULT_METER_NAME}. */
  meter?: string;
}

/** Options of {@link histogram}. */
export interface HistogramOptions extends InstrumentOptions {
  /** Explicit bucket boundaries, ascending (advice to the SDK). */
  boundaries?: number[];
}

/** Creates instruments on meters of one provider (or the global one). */
export interface MetricsApi {
  /** A meter from this provider. */
  meter(name?: string, version?: string): Meter;
  /** Monotonic counter (OTLP `sum`, `isMonotonic: true`, cumulative). */
  counter(name: string, options?: InstrumentOptions): Counter;
  /** Up/down counter (OTLP `sum`, non-monotonic). */
  upDownCounter(name: string, options?: InstrumentOptions): UpDownCounter;
  /** Histogram with optional explicit bucket boundaries. */
  histogram(name: string, options?: HistogramOptions): Histogram;
  /** Synchronous gauge (OTLP `gauge`, last value). */
  gauge(name: string, options?: InstrumentOptions): Gauge;
}

function metricOptions(options: InstrumentOptions): MetricOptions {
  const out: MetricOptions = {};
  if (options.description !== undefined) out.description = options.description;
  if (options.unit !== undefined) out.unit = options.unit;
  return out;
}

/** Instrument helpers bound to a meter source. */
export function metricsApi(getMeter: (name: string, version?: string) => Meter): MetricsApi {
  const meterFor = (o: InstrumentOptions): Meter => getMeter(o.meter ?? DEFAULT_METER_NAME);
  return {
    meter: (name = DEFAULT_METER_NAME, version?: string) => getMeter(name, version),
    counter: (name, options = {}) => meterFor(options).createCounter(name, metricOptions(options)),
    upDownCounter: (name, options = {}) =>
      meterFor(options).createUpDownCounter(name, metricOptions(options)),
    histogram: (name, options = {}) => {
      const opts = metricOptions(options);
      if (options.boundaries !== undefined)
        opts.advice = { explicitBucketBoundaries: [...options.boundaries] };
      return meterFor(options).createHistogram(name, opts);
    },
    gauge: (name, options = {}) => meterFor(options).createGauge(name, metricOptions(options)),
  };
}

const globalApi = metricsApi((name, version) => metrics.getMeter(name, version));

/** A meter from the global MeterProvider (set by `initTelemetry` unless `setGlobal: false`). */
export function meter(name: string = DEFAULT_METER_NAME, version?: string): Meter {
  return globalApi.meter(name, version);
}

/** Monotonic counter on the global MeterProvider. */
export function counter(name: string, options: InstrumentOptions = {}): Counter {
  return globalApi.counter(name, options);
}

/** Up/down counter on the global MeterProvider. */
export function upDownCounter(name: string, options: InstrumentOptions = {}): UpDownCounter {
  return globalApi.upDownCounter(name, options);
}

/** Histogram on the global MeterProvider. */
export function histogram(name: string, options: HistogramOptions = {}): Histogram {
  return globalApi.histogram(name, options);
}

/** Synchronous gauge on the global MeterProvider. */
export function gauge(name: string, options: InstrumentOptions = {}): Gauge {
  return globalApi.gauge(name, options);
}

/**
 * The official OTLP metric exporter for a collector config: `http/protobuf` →
 * `@opentelemetry/exporter-metrics-otlp-proto` (`{metricsUrl}`), `grpc` →
 * `@opentelemetry/exporter-metrics-otlp-grpc`. Cumulative temporality.
 */
export function createMetricExporter(config: OtelCollectorConfig): PushMetricExporter {
  const url = config.endpointFor("metrics");
  const tls = loadTls(config.tls);
  const compression = compressionAlgorithm(config.compression);
  const headers = config.headersFor("metrics");
  if (config.protocol === "grpc") {
    return new OtlpGrpcMetricExporter({
      url,
      metadata: grpcMetadata(headers),
      credentials: grpcChannelCredentials(url, tls),
      compression,
      timeoutMillis: config.exportTimeoutMs,
      userAgent: USER_AGENT,
    });
  }
  const agent = httpAgentOptions(tls);
  return new OtlpProtoMetricExporter({
    url,
    headers,
    compression,
    timeoutMillis: config.exportTimeoutMs,
    userAgent: USER_AGENT,
    ...(agent === undefined ? {} : { httpAgentOptions: agent }),
  });
}

/** Options of {@link createMeterProvider}. */
export interface CreateMeterProviderOptions {
  /** Resource of every metric. */
  resource: Resource;
  /** Replaces the OTLP exporter (tests). */
  exporter?: PushMetricExporter;
  /** Overrides `metricExportIntervalMs`. */
  exportIntervalMs?: number;
}

/** MeterProvider with a periodic reader (`metricExportIntervalMs`) and the OTLP exporter. */
export function createMeterProvider(
  config: OtelCollectorConfig,
  options: CreateMeterProviderOptions,
): MeterProvider {
  const interval = options.exportIntervalMs ?? config.metricExportIntervalMs;
  if (!Number.isFinite(interval) || interval <= 0)
    throw new ConfigError("metrics: exportIntervalMs must be a positive number");
  const reader = new PeriodicExportingMetricReader({
    exporter: options.exporter ?? createMetricExporter(config),
    exportIntervalMillis: interval,
    exportTimeoutMillis: Math.min(config.exportTimeoutMs, interval),
  });
  return new MeterProvider({ resource: options.resource, readers: [reader] });
}

/** Stops the runtime metrics collection. */
export interface RuntimeMetricsHandle {
  stop(): void;
}

/**
 * Registers Node.js runtime metrics on `meter`: `process.memory.usage` (RSS, By),
 * `v8js.memory.heap.used` (By), `process.cpu.time` (s, `cpu.mode` user/system),
 * `nodejs.eventloop.delay.p99` (s) and `process.uptime` (s).
 */
export function registerRuntimeMetrics(meterInstance: Meter): RuntimeMetricsHandle {
  let loop: IntervalHistogram | undefined;
  try {
    loop = monitorEventLoopDelay({ resolution: 20 });
    loop.enable();
  } catch {
    loop = undefined;
  }
  const rss = meterInstance.createObservableGauge("process.memory.usage", {
    unit: "By",
    description: "Resident set size of the process.",
  });
  const heap = meterInstance.createObservableGauge("v8js.memory.heap.used", {
    unit: "By",
    description: "V8 heap memory in use.",
  });
  const cpu = meterInstance.createObservableCounter("process.cpu.time", {
    unit: "s",
    description: "CPU time used by the process.",
  });
  const delay = meterInstance.createObservableGauge("nodejs.eventloop.delay.p99", {
    unit: "s",
    description: "99th percentile of the event loop delay since the last collection.",
  });
  const uptime = meterInstance.createObservableGauge("process.uptime", {
    unit: "s",
    description: "Time since the process started.",
  });
  const observables = [rss, heap, cpu, delay, uptime];
  const callback: BatchObservableCallback = (result) => {
    const mem = process.memoryUsage();
    result.observe(rss, mem.rss);
    result.observe(heap, mem.heapUsed);
    const usage = process.cpuUsage();
    result.observe(cpu, usage.user / 1e6, { "cpu.mode": "user" });
    result.observe(cpu, usage.system / 1e6, { "cpu.mode": "system" });
    if (loop !== undefined) {
      const p99 = loop.percentile(99);
      if (Number.isFinite(p99) && p99 > 0) result.observe(delay, p99 / 1e9);
      loop.reset();
    }
    result.observe(uptime, process.uptime());
  };
  meterInstance.addBatchObservableCallback(callback, observables);
  let stopped = false;
  return {
    stop(): void {
      if (stopped) return;
      stopped = true;
      meterInstance.removeBatchObservableCallback(callback, observables);
      loop?.disable();
    },
  };
}
