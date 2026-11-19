// Service-owned: shared OTLP plumbing — exporter options (headers, compression, TLS, gRPC
// credentials) for the official OpenTelemetry exporters, and error mapping of OTLP responses
// (HTTP status + google.rpc.Status body, gRPC status codes) to the SDK error classes.

import { readFileSync } from "node:fs";
import { credentials as grpcCredentials, type ChannelCredentials, Metadata } from "@grpc/grpc-js";
import { CompressionAlgorithm } from "@opentelemetry/otlp-exporter-base";
import {
  CancelledError,
  ConfigError,
  ErrorCode,
  OtelCollectorError,
  type TlsOptions,
  UnavailableError,
  errorForCode,
  errorForStatus,
  reasonForCode,
  redactSecretsInText,
  redactText,
  LANGUAGE,
  PACKAGE_KEY,
  SDK_VERSION,
} from "./core/index.js";

/** `grow-codes-opentelemetry-collector/<version> (typescript)`, prepended to the exporters' UA. */
export const USER_AGENT = `grow-codes-${PACKAGE_KEY}/${SDK_VERSION} (${LANGUAGE})`;

/** gRPC method paths of the three OTLP export services. */
export const GRPC_EXPORT_PATHS = Object.freeze({
  traces: "/opentelemetry.proto.collector.trace.v1.TraceService/Export",
  logs: "/opentelemetry.proto.collector.logs.v1.LogsService/Export",
  metrics: "/opentelemetry.proto.collector.metrics.v1.MetricsService/Export",
});

/** TLS material loaded from {@link TlsOptions}. */
export interface TlsMaterial {
  ca?: Buffer;
  cert?: Buffer;
  key?: Buffer;
  insecureSkipVerify: boolean;
}

function readPem(path: string | undefined): Buffer | undefined {
  if (path === undefined) return undefined;
  try {
    return readFileSync(path);
  } catch {
    throw new ConfigError("otlp: cannot read a TLS file (check the configured path)");
  }
}

/** Loads the CA bundle and client certificate/key; `ConfigError` when a file is unreadable. */
export function loadTls(tls: Readonly<TlsOptions>): TlsMaterial {
  const out: TlsMaterial = { insecureSkipVerify: tls.insecureSkipVerify === true };
  const caPath = readPem(tls.caCertPath);
  const ca =
    tls.caCertPem !== undefined
      ? Buffer.concat([Buffer.from(tls.caCertPem), ...(caPath === undefined ? [] : [caPath])])
      : caPath;
  if (ca !== undefined) out.ca = ca;
  const cert = readPem(tls.clientCertPath);
  if (cert !== undefined) out.cert = cert;
  const key = readPem(tls.clientKeyPath);
  if (key !== undefined) out.key = key;
  return out;
}

/** `http.Agent` options for the OTLP/HTTP exporters, or `undefined` when no TLS setting applies. */
export function httpAgentOptions(tls: TlsMaterial): Record<string, unknown> | undefined {
  const agent: Record<string, unknown> = {};
  if (tls.ca !== undefined) agent.ca = tls.ca;
  if (tls.cert !== undefined) agent.cert = tls.cert;
  if (tls.key !== undefined) agent.key = tls.key;
  if (tls.insecureSkipVerify) agent.rejectUnauthorized = false;
  return Object.keys(agent).length > 0 ? agent : undefined;
}

/**
 * gRPC channel credentials: TLS for an `https://` target or when any TLS material is set,
 * otherwise plaintext.
 */
export function grpcChannelCredentials(url: string, tls: TlsMaterial): ChannelCredentials {
  const secure =
    url.startsWith("https:") ||
    tls.ca !== undefined ||
    tls.cert !== undefined ||
    tls.insecureSkipVerify;
  if (!secure) return grpcCredentials.createInsecure();
  return grpcCredentials.createSsl(
    tls.ca ?? null,
    tls.key ?? null,
    tls.cert ?? null,
    tls.insecureSkipVerify
      ? { checkServerIdentity: () => undefined, rejectUnauthorized: false }
      : {},
  );
}

/** gRPC metadata from export headers (names lower-cased). */
export function grpcMetadata(headers: Readonly<Record<string, string>>): Metadata {
  const metadata = new Metadata();
  for (const [k, v] of Object.entries(headers)) metadata.set(k.toLowerCase(), v);
  return metadata;
}

/** `host:port` of a gRPC URL (`http://collector:4317` → `collector:4317`). */
export function grpcTarget(url: string): string {
  const u = new URL(url);
  const port = u.port !== "" ? u.port : u.protocol === "https:" ? "443" : "80";
  return `${u.hostname}:${port}`;
}

/** Exporter compression enum for a config value. */
export function compressionAlgorithm(value: "gzip" | "none"): CompressionAlgorithm {
  return value === "gzip" ? CompressionAlgorithm.GZIP : CompressionAlgorithm.NONE;
}

function readVarint(bytes: Uint8Array, pos: number): [number, number] | undefined {
  let value = 0;
  let shift = 0;
  for (let i = pos; i < bytes.length && shift < 35; i++) {
    const b = bytes[i] as number;
    value += (b & 0x7f) * 2 ** shift;
    if ((b & 0x80) === 0) return [value, i + 1];
    shift += 7;
  }
  return undefined;
}

/**
 * Decodes a binary `google.rpc.Status` (field 1 `code`, field 2 `message`); `undefined` when the
 * bytes are not a well-formed Status.
 */
export function decodeRpcStatus(
  bytes: Uint8Array,
): { code?: number; message?: string } | undefined {
  const out: { code?: number; message?: string } = {};
  let pos = 0;
  while (pos < bytes.length) {
    const tag = readVarint(bytes, pos);
    if (tag === undefined) return undefined;
    pos = tag[1];
    const field = Math.floor(tag[0] / 8);
    const wire = tag[0] % 8;
    if (wire === 0) {
      const v = readVarint(bytes, pos);
      if (v === undefined) return undefined;
      if (field === 1) out.code = v[0];
      pos = v[1];
    } else if (wire === 2) {
      const len = readVarint(bytes, pos);
      if (len === undefined || len[1] + len[0] > bytes.length) return undefined;
      if (field === 2)
        out.message = new TextDecoder().decode(bytes.subarray(len[1], len[1] + len[0]));
      pos = len[1] + len[0];
    } else {
      return undefined;
    }
  }
  return out;
}

/**
 * Human text of an OTLP error body: JSON `{"code","message"}`, a binary `google.rpc.Status`, or
 * plain text. Redacted and truncated to 300 characters; `undefined` for an empty body.
 */
export function otlpErrorText(
  body: string | Uint8Array | undefined,
  knownSecrets: readonly string[] = [],
): string | undefined {
  if (body === undefined || body.length === 0) return undefined;
  const bytes = typeof body === "string" ? Buffer.from(body, "utf8") : body;
  const text = typeof body === "string" ? body : new TextDecoder().decode(body);
  let message: string | undefined;
  try {
    const parsed = JSON.parse(text) as { message?: unknown };
    if (typeof parsed.message === "string") message = parsed.message;
  } catch {
    const status = decodeRpcStatus(bytes);
    message = status?.message ?? text.replace(/[^\x20-\x7e]+/g, " ");
  }
  const clean = message?.replace(/\s+/g, " ").trim();
  if (clean === undefined || clean === "") return undefined;
  const redacted = redactSecretsInText(redactText(clean, knownSecrets));
  return redacted.length > 300 ? `${redacted.slice(0, 299)}…` : redacted;
}

/** gRPC status code names (index = code). */
const GRPC_NAMES = [
  "OK",
  "CANCELLED",
  "UNKNOWN",
  "INVALID_ARGUMENT",
  "DEADLINE_EXCEEDED",
  "NOT_FOUND",
  "ALREADY_EXISTS",
  "PERMISSION_DENIED",
  "RESOURCE_EXHAUSTED",
  "FAILED_PRECONDITION",
  "ABORTED",
  "OUT_OF_RANGE",
  "UNIMPLEMENTED",
  "INTERNAL",
  "UNAVAILABLE",
  "DATA_LOSS",
  "UNAUTHENTICATED",
];

/** Maps a gRPC status code to an SDK {@link ErrorCode}. */
export function codeForGrpcStatus(status: number): ErrorCode {
  switch (status) {
    case 1:
      return ErrorCode.Cancelled;
    case 3:
    case 9:
    case 11:
    case 13:
      return ErrorCode.Validation;
    case 4:
      return ErrorCode.Timeout;
    case 5:
    case 12:
      return ErrorCode.NotFound;
    case 6:
    case 10:
      return ErrorCode.Conflict;
    case 7:
      return ErrorCode.PermissionDenied;
    case 8:
      return ErrorCode.RateLimited;
    case 16:
      return ErrorCode.Authentication;
    default:
      return ErrorCode.Unavailable;
  }
}

/** Builds the SDK error for a gRPC status: `"<op>: <message> (gRPC <NAME>)"`. */
export function errorForGrpcStatus(
  operation: string,
  status: number,
  message: string | undefined,
  knownSecrets: readonly string[] = [],
  retryable?: boolean,
): OtelCollectorError {
  const code = codeForGrpcStatus(status);
  const text = otlpErrorText(message, knownSecrets) ?? reasonForCode(code);
  const name = GRPC_NAMES[status] ?? String(status);
  return errorForCode(code, `${operation}: ${text} (gRPC ${name})`, {
    ...(retryable === undefined ? {} : { retryable }),
  });
}

/**
 * Converts an error reported by an official OTLP exporter into an SDK error. The exporters
 * already retried (OTLP rules), so the result is never marked retryable: callers must not wrap
 * the export in a second retry loop.
 */
export function mapExportError(
  operation: string,
  error: unknown,
  knownSecrets: readonly string[] = [],
): OtelCollectorError {
  if (error instanceof OtelCollectorError) return error;
  const e = (error ?? {}) as {
    code?: unknown;
    data?: unknown;
    message?: unknown;
    details?: unknown;
  };
  const isGrpc = typeof e.details === "string" && typeof e.code === "number";
  if (typeof e.code === "number" && e.code >= 100) {
    const reason = otlpErrorText(typeof e.data === "string" ? e.data : undefined, knownSecrets);
    return errorForStatus(operation, e.code, {
      ...(reason === undefined ? {} : { reason }),
      retryable: false,
    });
  }
  if (isGrpc && typeof e.code === "number") {
    return errorForGrpcStatus(
      operation,
      e.code,
      typeof e.details === "string" ? e.details : undefined,
      knownSecrets,
      false,
    );
  }
  const raw = typeof e.message === "string" ? e.message : String(error);
  const reason = redactSecretsInText(redactText(raw, knownSecrets)).slice(0, 300);
  if (/timed? ?out|deadline/i.test(reason))
    return errorForCode(ErrorCode.Timeout, `${operation}: ${reason}`, { retryable: false });
  return new UnavailableError(`${operation}: ${reason}`, { retryable: false });
}

/**
 * Like the core `sleep`, but the timer keeps the process alive: a short script awaiting a
 * retrying call must not exit in the middle of a backoff (the core sleep unrefs its timer).
 */
export function refSleep(ms: number, signal?: AbortSignal, operation = "sleep"): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    if (signal?.aborted === true) {
      reject(new CancelledError(`${operation}: cancelled`));
      return;
    }
    const onAbort = (): void => {
      clearTimeout(timer);
      reject(new CancelledError(`${operation}: cancelled`));
    };
    const timer = setTimeout(
      () => {
        signal?.removeEventListener("abort", onAbort);
        resolve();
      },
      Math.max(0, ms),
    );
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}
