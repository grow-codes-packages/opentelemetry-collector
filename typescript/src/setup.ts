// Service-owned: tracing preset for the OpenTelemetry Collector — OTLP span exporter (official
// http/protobuf or gRPC exporter) and `initTracing` with collector defaults from the environment.

import { OTLPTraceExporter as OtlpGrpcTraceExporter } from "@opentelemetry/exporter-trace-otlp-grpc";
import { OTLPTraceExporter as OtlpProtoTraceExporter } from "@opentelemetry/exporter-trace-otlp-proto";
import type { SpanExporter } from "@opentelemetry/sdk-trace-base";
import { ConfigError, definedOnly, type EnvMap } from "./core/index.js";
import { OtelCollectorConfig, type OtelCollectorConfigOptions } from "./config.js";
import {
  compressionAlgorithm,
  grpcChannelCredentials,
  grpcMetadata,
  httpAgentOptions,
  loadTls,
  USER_AGENT,
} from "./otlp.js";
import {
  initTracingWith,
  type InitTracingOptions,
  type ResolvedTracingConfig,
  type TracingConfig,
  type TracingHandle,
} from "./tracing/index.js";

/** {@link TracingConfig} where `serviceName` may come from the collector config/environment. */
export type CollectorTracingConfig = Omit<TracingConfig, "serviceName"> & { serviceName?: string };

/** Options of {@link initTracing}. */
export interface CollectorInitTracingOptions extends Omit<InitTracingOptions, "defaults"> {
  /**
   * Collector settings that supply every field the tracing config leaves unset (endpoint,
   * protocol, headers, compression, TLS, sampler, resource, batch). Default:
   * `OtelCollectorConfig.fromEnv({}, env)`.
   */
  collector?: OtelCollectorConfig | OtelCollectorConfigOptions;
  /** Environment for the default collector config; `false` ignores the environment. */
  env?: EnvMap | false;
}

/** A running collector tracing setup. */
export interface CollectorTracingHandle extends TracingHandle {
  /** The exporter endpoint in use. */
  readonly endpoint: string;
}

function hasPath(url: string): boolean {
  try {
    const path = new URL(url).pathname;
    return path !== "" && path !== "/";
  } catch {
    return true;
  }
}

/**
 * The traces exporter URL: for `http/protobuf` an endpoint without a path (a base such as
 * `http://collector:4318`) gets `/v1/traces`; anything else is used as-is.
 */
export function resolveTracesEndpoint(protocol: string, endpoint: string): string {
  return protocol !== "grpc" && !hasPath(endpoint)
    ? `${endpoint.replace(/\/+$/, "")}/v1/traces`
    : endpoint;
}

/**
 * Builds the OTLP span exporter for a resolved tracing config (the `exporterFactory` of
 * `initTracingWith`): `http/protobuf` → `@opentelemetry/exporter-trace-otlp-proto`, `grpc` →
 * `@opentelemetry/exporter-trace-otlp-grpc`. `native` throws `ConfigError`. The export deadline
 * is `batch.exportTimeoutMs`; retries follow the OTLP rules of the official exporter.
 */
export function collectorSpanExporterFactory(config: ResolvedTracingConfig): SpanExporter {
  if (config.protocol === "native")
    throw new ConfigError(
      "tracing: the OpenTelemetry Collector has no native protocol; use grpc or http/protobuf",
    );
  const endpoint =
    config.endpoint ??
    (config.protocol === "grpc"
      ? OtelCollectorConfig.DEFAULT_GRPC_URL
      : `${OtelCollectorConfig.DEFAULT_URL}/v1/traces`);
  const url = resolveTracesEndpoint(config.protocol, endpoint);
  const tls = loadTls(config.tls);
  const compression = compressionAlgorithm(config.compression);
  const timeoutMillis = config.batch.exportTimeoutMs;
  if (config.protocol === "grpc") {
    return new OtlpGrpcTraceExporter({
      url,
      metadata: grpcMetadata(config.headers),
      credentials: grpcChannelCredentials(url, tls),
      compression,
      timeoutMillis,
      userAgent: USER_AGENT,
    });
  }
  const agent = httpAgentOptions(tls);
  return new OtlpProtoTraceExporter({
    url,
    headers: { ...config.headers },
    compression,
    timeoutMillis,
    userAgent: USER_AGENT,
    ...(agent === undefined ? {} : { httpAgentOptions: agent }),
  });
}

/** Tracing defaults derived from a collector config (used beneath an explicit tracing config). */
export function tracingDefaults(collector: OtelCollectorConfig): TracingConfig {
  const defaults: TracingConfig = {
    serviceName: collector.serviceName,
    resourceAttributes: { ...collector.resourceAttributes },
    endpoint: collector.endpointFor("traces"),
    protocol: collector.protocol,
    headers: collector.headersFor("traces"),
    sampler: collector.sampler,
    samplerRatio: collector.samplerRatio,
    propagators: [...collector.propagators],
    batch: { exportTimeoutMs: collector.exportTimeoutMs, ...collector.tracesBatch },
    tls: { ...collector.tls },
    compression: collector.compression,
  };
  if (collector.serviceVersion !== undefined) defaults.serviceVersion = collector.serviceVersion;
  if (collector.environment !== undefined) defaults.environment = collector.environment;
  return defaults;
}

/**
 * Sets up OpenTelemetry tracing that exports to an OpenTelemetry Collector (SPEC §5): OTLP
 * `http/protobuf` to `http://127.0.0.1:4318/v1/traces` by default, or `grpc` to
 * `http://127.0.0.1:4317`. Unset fields come from the collector config (by default
 * `OtelCollectorConfig.fromEnv()`, i.e. `GROW_OPENTELEMETRY_COLLECTOR_*`, `GROW_OTEL_*` and the
 * standard `OTEL_*` variables). Resource attributes merge key by key (explicit wins).
 */
export function initTracing(
  config: CollectorTracingConfig = {},
  options: CollectorInitTracingOptions = {},
): CollectorTracingHandle {
  const { collector, env, ...initOptions } = options;
  const cfg =
    collector !== undefined
      ? OtelCollectorConfig.from(collector)
      : env === false
        ? new OtelCollectorConfig()
        : OtelCollectorConfig.fromEnv({}, env ?? process.env);
  const defaults = tracingDefaults(cfg);
  const explicit = { ...config };
  if (explicit.serviceName === undefined || explicit.serviceName.trim() === "")
    delete explicit.serviceName;
  const merged: TracingConfig = {
    ...defaults,
    ...definedOnly(explicit),
    resourceAttributes: { ...defaults.resourceAttributes, ...config.resourceAttributes },
    headers: { ...defaults.headers, ...config.headers },
    batch: { ...defaults.batch, ...config.batch },
    tls: { ...defaults.tls, ...config.tls },
  };
  if (config.endpoint !== undefined)
    merged.endpoint = resolveTracesEndpoint(merged.protocol ?? cfg.protocol, config.endpoint);
  else if (config.protocol !== undefined && config.protocol !== cfg.protocol)
    merged.endpoint = config.protocol === "grpc" ? cfg.grpcUrl : cfg.signalUrl("traces");
  const handle = initTracingWith(merged, collectorSpanExporterFactory, initOptions);
  return {
    ...handle,
    endpoint: resolveTracesEndpoint(handle.config.protocol, handle.config.endpoint as string),
  };
}
