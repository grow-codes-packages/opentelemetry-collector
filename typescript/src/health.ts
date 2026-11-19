// Service-owned: collector health (health_check extension on :13133, zpages fallback on :55679)
// and the admin reads used by serverVersion (zpages servicez, internal telemetry target_info).

import {
  CancelledError,
  type HealthStatus,
  HttpTransport,
  type TransportOptions,
  UnsupportedError,
  healthStatus,
  pollUntil,
  probeHealth,
} from "./core/index.js";
import { OtelCollectorConfig, type OtelCollectorConfigOptions } from "./config.js";
import { refSleep } from "./otlp.js";

/** Per-call options. */
export interface CallOptions {
  /** Cancels the call (`CancelledError`). */
  signal?: AbortSignal;
}

/** Options of {@link CollectorHealthClient.waitUntilReady}. */
export interface WaitUntilReadyOptions extends CallOptions {
  /** Pause between probes in ms. Default 500. */
  intervalMs?: number;
}

/** Test seams (undici `MockAgent`, fake sleep). Not part of the stable API. */
export type ClientInternals = Pick<TransportOptions, "dispatcher" | "sleep" | "now">;

const VERSION_ROW = /<b>Version<\/b><\/td>\s*<td>[^<]*<\/td>\s*<td[^>]*>([^<]+)<\/td>/;
const TARGET_INFO = /^target_info\{[^}]*\bservice_version="([^"]+)"/m;

/** Parses the collector version from the zpages `/debug/servicez` HTML; `undefined` if absent. */
export function parseServicezVersion(html: string): string | undefined {
  return VERSION_ROW.exec(html)?.[1]?.trim() || undefined;
}

/** Parses `service_version` from a Prometheus `target_info` series; `undefined` if absent. */
export function parseTargetInfoVersion(metrics: string): string | undefined {
  return TARGET_INFO.exec(metrics)?.[1] || undefined;
}

/**
 * Health of an OpenTelemetry Collector: `GET {healthUrl}` (health_check extension) and, when that
 * endpoint is unreachable (stock stacks bind it to localhost), `GET {zpagesUrl}/debug/servicez`.
 * Admin endpoints are unauthenticated: the configured export headers are never sent to them.
 */
export class CollectorHealthClient {
  /** The immutable configuration. */
  readonly config: OtelCollectorConfig;
  readonly #transport: HttpTransport;
  readonly #sleep: (ms: number, signal?: AbortSignal) => Promise<void>;
  #closed = false;

  /** Accepts a {@link OtelCollectorConfig} or plain options. */
  constructor(config: OtelCollectorConfigOptions = {}, internals: ClientInternals = {}) {
    const cfg = OtelCollectorConfig.from(config);
    this.config = cfg;
    this.#sleep = internals.sleep ?? refSleep;
    this.#transport = new HttpTransport({
      ...internals,
      sleep: this.#sleep,
      config: {
        url: cfg.zpagesUrl,
        connectTimeoutMs: cfg.connectTimeoutMs,
        requestTimeoutMs: cfg.requestTimeoutMs,
        totalTimeoutMs: cfg.totalTimeoutMs,
        retry: cfg.retry,
        tls: cfg.tls,
        headers: {},
        ...(cfg.proxy === undefined ? {} : { proxy: cfg.proxy }),
        ...(cfg.userAgentSuffix === undefined ? {} : { userAgentSuffix: cfg.userAgentSuffix }),
        ...(cfg.hooks === undefined ? {} : { hooks: cfg.hooks }),
        ...(cfg.logger === undefined ? {} : { logger: cfg.logger }),
      },
      knownSecrets: () => cfg.knownSecrets(),
    });
  }

  /** Builds a client from the environment (see {@link OtelCollectorConfig.fromEnv}). */
  static fromEnv(overrides: OtelCollectorConfigOptions = {}): CollectorHealthClient {
    return new CollectorHealthClient(OtelCollectorConfig.fromEnv(overrides));
  }

  /**
   * Collector health. `200 {"status":"Server available",…}` → `healthy=true, status="ok"` with
   * `details` `{source: "health_check", vendorStatus, upSince, uptime}`; `503` → `unhealthy`;
   * unreachable → zpages fallback (`details.source = "zpages"`), else `unreachable`. Never throws
   * for an unhealthy or unreachable collector (only `CANCELLED` and `CONFIG`).
   */
  async health(options: CallOptions = {}): Promise<HealthStatus> {
    if (this.#closed) throw new CancelledError("health: client is closed");
    const primary = await probeHealth(async () => {
      const res = await this.#transport.get<Record<string, unknown> | undefined>(
        this.config.healthUrl,
        { operation: "health", allowStatus: [503], ...options },
      );
      if (res.status === 503)
        return healthStatus("unhealthy", { source: "health_check", httpStatus: 503 }, false);
      const { status, ...rest } = res.data ?? {};
      return healthStatus(
        "ok",
        {
          source: "health_check",
          ...(typeof status === "string" ? { vendorStatus: status } : {}),
          ...rest,
        },
        true,
      );
    });
    if (primary.status !== "unreachable") return primary;
    const fallback = await probeHealth(async () => {
      await this.#transport.get(`${this.config.zpagesUrl.replace(/\/+$/, "")}/debug/servicez`, {
        operation: "health",
        responseType: "none",
        ...options,
      });
      return healthStatus("ok", { source: "zpages" }, true);
    });
    return fallback.healthy ? fallback : primary;
  }

  /**
   * Polls {@link health} until the collector is healthy and returns that status. Throws
   * `TimeoutError` after `timeoutMs` (default 30 000) and `CancelledError` on abort.
   */
  waitUntilReady(timeoutMs = 30_000, options: WaitUntilReadyOptions = {}): Promise<HealthStatus> {
    const poll: Parameters<typeof pollUntil>[2] = {
      timeoutMs,
      intervalMs: options.intervalMs ?? 500,
      operation: "waitUntilReady",
      sleep: this.#sleep,
    };
    if (options.signal !== undefined) poll.signal = options.signal;
    const call: CallOptions = options.signal === undefined ? {} : { signal: options.signal };
    return pollUntil(
      () => this.health(call),
      (h) => h.healthy,
      poll,
    );
  }

  /**
   * Collector version: zpages `/debug/servicez` "Version" row, else `service_version` of
   * `target_info` at {@link OtelCollectorConfig.telemetryUrl}. `UnsupportedError` when neither
   * answers with a version.
   */
  async serverVersion(options: CallOptions = {}): Promise<string> {
    if (this.#closed) throw new CancelledError("serverVersion: client is closed");
    let cause: unknown;
    try {
      const res = await this.#transport.get<string>(
        `${this.config.zpagesUrl.replace(/\/+$/, "")}/debug/servicez`,
        { operation: "serverVersion", responseType: "text", allowStatus: [404], ...options },
      );
      const version = res.status === 200 ? parseServicezVersion(res.data) : undefined;
      if (version !== undefined) return version;
    } catch (err) {
      if ((err as { code?: string }).code === "CANCELLED") throw err;
      cause = err;
    }
    if (this.config.telemetryUrl !== undefined) {
      try {
        const res = await this.#transport.get<string>(this.config.telemetryUrl, {
          operation: "serverVersion",
          responseType: "text",
          allowStatus: [404],
          ...options,
        });
        const version = res.status === 200 ? parseTargetInfoVersion(res.data) : undefined;
        if (version !== undefined) return version;
      } catch (err) {
        if ((err as { code?: string }).code === "CANCELLED") throw err;
        cause = err;
      }
    }
    throw new UnsupportedError(
      "serverVersion: the collector exposes no version (zpages and internal telemetry unavailable)",
      cause === undefined ? {} : { cause },
    );
  }

  /** Releases connections. Idempotent; later calls fail with `CancelledError`. */
  close(): Promise<void> {
    this.#closed = true;
    return this.#transport.close();
  }
}
