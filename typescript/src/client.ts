// Service-owned: the OpenTelemetry Collector client — health (health_check / zpages), an
// authenticated OTLP ping, the collector version, and factories for the OTLP exporters whose
// lifetime the client owns (close() flushes and shuts them down).

import { Client as GrpcClient, status as GrpcStatus, type ServiceError } from "@grpc/grpc-js";
import {
  CancelledError,
  type HealthStatus,
  HttpTransport,
  OtelCollectorError,
  TimeoutError,
} from "./core/index.js";
import { OtelCollectorConfig, type OtelCollectorConfigOptions } from "./config.js";
import {
  CollectorHealthClient,
  type CallOptions,
  type ClientInternals,
  type WaitUntilReadyOptions,
} from "./health.js";
import type { BatchingLogClient, LogQueryClient } from "./logging/index.js";
import { createLogClient, OtlpLogExporter, type CreateLogClientOptions } from "./logs.js";
import {
  errorForGrpcStatus,
  GRPC_EXPORT_PATHS,
  grpcChannelCredentials,
  grpcMetadata,
  grpcTarget,
  loadTls,
  refSleep,
  USER_AGENT,
} from "./otlp.js";
import { CollectorLogQueryClient, CollectorTraceQueryClient } from "./query.js";
import {
  initTracing,
  type CollectorInitTracingOptions,
  type CollectorTracingConfig,
  type CollectorTracingHandle,
} from "./setup.js";
import { initTelemetry, type InitTelemetryOptions, type TelemetryHandle } from "./telemetry.js";
import type { TraceQueryClient } from "./tracing/index.js";

export type { CallOptions, ClientInternals, WaitUntilReadyOptions } from "./health.js";

/** Every call on a closed client fails with `CANCELLED` (same in all five languages). */
function closedError(operation: string): CancelledError {
  return new CancelledError(`${operation}: client is closed`);
}

interface Closeable {
  close?(): Promise<void>;
  shutdown?(): Promise<void>;
}

/** Client for an OpenTelemetry Collector. */
export class OtelCollectorClient {
  /** The immutable configuration. */
  readonly config: OtelCollectorConfig;
  readonly #transport: HttpTransport;
  readonly #health: CollectorHealthClient;
  readonly #owned = new Set<Closeable>();
  readonly #internals: ClientInternals;
  #closing: Promise<void> | null = null;

  /** Accepts a {@link OtelCollectorConfig} or plain options. */
  constructor(config: OtelCollectorConfigOptions = {}, internals: ClientInternals = {}) {
    const cfg = OtelCollectorConfig.from(config);
    this.config = cfg;
    this.#internals = internals;
    this.#transport = new HttpTransport({
      ...internals,
      sleep: internals.sleep ?? refSleep,
      config: cfg,
      knownSecrets: () => cfg.knownSecrets(),
    });
    this.#health = new CollectorHealthClient(cfg, internals);
  }

  /**
   * Builds a client from `GROW_OPENTELEMETRY_COLLECTOR_*`, `GROW_OTEL_*` and the standard
   * `OTEL_*` variables (see {@link OtelCollectorConfig.fromEnv}).
   */
  static fromEnv(overrides: OtelCollectorConfigOptions = {}): OtelCollectorClient {
    return new OtelCollectorClient(OtelCollectorConfig.fromEnv(overrides));
  }

  /** The shared HTTP transport (for service modules). @internal */
  get transport(): HttpTransport {
    return this.#transport;
  }

  /** The health client (health_check extension with zpages fallback). */
  get healthClient(): CollectorHealthClient {
    return this.#health;
  }

  /**
   * Unauthenticated health: `GET {healthUrl}` (port 13133), falling back to zpages when the
   * health endpoint is unreachable. Never throws for an unhealthy or unreachable collector.
   */
  health(options: CallOptions = {}): Promise<HealthStatus> {
    if (this.#closing !== null) return Promise.reject(closedError("health"));
    return this.#health.health(options);
  }

  /** Polls {@link health} until healthy; `TimeoutError` after `timeoutMs` (default 30 000). */
  waitUntilReady(timeoutMs?: number, options: WaitUntilReadyOptions = {}): Promise<HealthStatus> {
    if (this.#closing !== null) return Promise.reject(closedError("waitUntilReady"));
    return this.#health.waitUntilReady(timeoutMs, options);
  }

  /**
   * Authenticated round trip: an empty OTLP trace export (creates no telemetry) with the
   * configured headers — `POST {tracesUrl}` with `{}` (HTTP) or `TraceService/Export` with an
   * empty message to `endpointFor("traces")` (gRPC). Returns the latency in ms. `AuthenticationError` on 401 / gRPC
   * UNAUTHENTICATED, `PermissionDeniedError` on 403, `NotFoundError` for a wrong path.
   */
  async ping(options: CallOptions = {}): Promise<number> {
    if (this.#closing !== null) throw closedError("ping");
    const start = performance.now();
    if (this.config.protocol === "grpc") await this.#grpcPing(options);
    else
      await this.#transport.post(this.config.signalUrl("traces"), {
        operation: "ping",
        json: {},
        headers: this.config.tracesHeaders,
        idempotent: true,
        responseType: "none",
        ...options,
      });
    return performance.now() - start;
  }

  async #grpcPing(options: CallOptions): Promise<void> {
    const cfg = this.config;
    const secrets = cfg.knownSecrets();
    const target = cfg.endpointFor("traces");
    const client = new GrpcClient(
      grpcTarget(target),
      grpcChannelCredentials(target, loadTls(cfg.tls)),
      { "grpc.primary_user_agent": USER_AGENT },
    );
    const sleepFn = this.#internals.sleep ?? refSleep;
    try {
      for (let attempt = 1; ; attempt++) {
        if (options.signal?.aborted === true) throw new CancelledError("ping: cancelled");
        const error = await new Promise<ServiceError | null>((resolve) => {
          const call = client.makeUnaryRequest(
            GRPC_EXPORT_PATHS.traces,
            (b: Buffer) => b,
            (b: Buffer) => b,
            Buffer.alloc(0),
            grpcMetadata(cfg.headersFor("traces")),
            { deadline: Date.now() + cfg.requestTimeoutMs },
            (err) => {
              options.signal?.removeEventListener("abort", onAbort);
              resolve(err ?? null);
            },
          );
          const onAbort = (): void => {
            call.cancel();
          };
          options.signal?.addEventListener("abort", onAbort, { once: true });
        });
        if (error === null) return;
        if (error.code === GrpcStatus.CANCELLED) throw new CancelledError("ping: cancelled");
        if (error.code === GrpcStatus.DEADLINE_EXCEEDED)
          throw new TimeoutError(`ping: request timed out after ${cfg.requestTimeoutMs} ms`);
        const mapped = errorForGrpcStatus("ping", error.code, error.details, secrets);
        if (!mapped.retryable || attempt > cfg.retry.maxRetries) throw mapped;
        await sleepFn(cfg.retry.backoffMs(attempt - 1), options.signal).catch((err: unknown) => {
          throw err instanceof OtelCollectorError ? err : new CancelledError("ping: cancelled");
        });
      }
    } finally {
      client.close();
    }
  }

  /**
   * Collector version from zpages `/debug/servicez` (works on stock stacks), else `target_info`
   * at `telemetryUrl`; `UnsupportedError` when neither is reachable.
   */
  serverVersion(options: CallOptions = {}): Promise<string> {
    if (this.#closing !== null) return Promise.reject(closedError("serverVersion"));
    return this.#health.serverVersion(options);
  }

  #own<T extends Closeable>(resource: T): T {
    if (this.#closing !== null) {
      void (resource.shutdown?.() ?? resource.close?.());
      throw closedError("client");
    }
    this.#owned.add(resource);
    return resource;
  }

  /** {@link initTelemetry} with this client's config; `close()` shuts the handle down. */
  initTelemetry(options: Omit<InitTelemetryOptions, "config" | "env"> = {}): TelemetryHandle {
    return this.#own(initTelemetry({ ...options, config: this.config }));
  }

  /** {@link initTracing} with this client's config as defaults; `close()` shuts it down. */
  initTracing(
    config: CollectorTracingConfig = {},
    options: Omit<CollectorInitTracingOptions, "collector" | "env"> = {},
  ): CollectorTracingHandle {
    return this.#own(initTracing(config, { ...options, collector: this.config }));
  }

  /** An OTLP {@link LogClient}; `close()` flushes and closes it. */
  logClient(options: CreateLogClientOptions = {}): BatchingLogClient {
    return this.#own(createLogClient(this.config, options));
  }

  /** A bare OTLP log exporter for this config; `close()` shuts it down. */
  logExporter(): OtlpLogExporter {
    return this.#own(new OtlpLogExporter(this.config));
  }

  /** Trace query client: the collector has none, every method throws `UnsupportedError`. */
  traceQuery(): TraceQueryClient {
    return new CollectorTraceQueryClient();
  }

  /** Log query client: the collector has none, every method throws `UnsupportedError`. */
  logQuery(): LogQueryClient {
    return new CollectorLogQueryClient();
  }

  /**
   * Flushes and shuts down every handle, log client and exporter this client created, then
   * releases connections. Idempotent.
   */
  close(): Promise<void> {
    this.#closing ??= (async () => {
      const owned = [...this.#owned];
      this.#owned.clear();
      await Promise.allSettled(
        owned.map((r) =>
          r.shutdown !== undefined ? r.shutdown() : (r.close?.() ?? Promise.resolve()),
        ),
      );
      await Promise.all([this.#transport.close(), this.#health.close()]);
    })();
    return this.#closing;
  }
}
