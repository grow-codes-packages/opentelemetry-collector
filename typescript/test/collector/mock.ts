// Raw-body mock OpenTelemetry Collector for unit tests (binary protobuf, gzip) + fixture loading.

import { readFileSync } from "node:fs";
import { createServer, type IncomingHttpHeaders, type Server } from "node:http";
import type { AddressInfo, Socket } from "node:net";
import { gunzipSync } from "node:zlib";

export interface RawRequest {
  method: string;
  url: string;
  headers: IncomingHttpHeaders;
  /** Body after gzip decoding. */
  body: Buffer;
  gzip: boolean;
}

export interface RawResponse {
  status?: number;
  headers?: Record<string, string>;
  body?: string | Buffer | object;
  delayMs?: number;
}

export interface MockCollector {
  url: string;
  port: number;
  requests: RawRequest[];
  close(): Promise<void>;
}

/** `0a00`: an empty `partial_success` (OTLP success body). */
export const OTLP_OK = Buffer.from("0a00", "hex");

/** Starts a raw HTTP server; `handler` may be a function or a list (last repeats). */
export async function startCollector(
  handler: ((req: RawRequest, i: number) => RawResponse) | RawResponse[] = () => ({
    body: OTLP_OK,
  }),
): Promise<MockCollector> {
  const requests: RawRequest[] = [];
  const sockets = new Set<Socket>();
  const pick = Array.isArray(handler)
    ? (_r: RawRequest, i: number) => handler[Math.min(i, handler.length - 1)] ?? {}
    : handler;
  const server: Server = createServer((req, res) => {
    const chunks: Buffer[] = [];
    req.on("data", (c: Buffer) => chunks.push(c));
    req.on("end", () => {
      const raw = Buffer.concat(chunks);
      const gzip = req.headers["content-encoding"] === "gzip";
      const recorded: RawRequest = {
        method: req.method ?? "",
        url: req.url ?? "",
        headers: req.headers,
        body: gzip ? gunzipSync(raw) : raw,
        gzip,
      };
      const index = requests.length;
      requests.push(recorded);
      const r = pick(recorded, index);
      const finish = (): void => {
        const isObject =
          r.body !== undefined && typeof r.body === "object" && !Buffer.isBuffer(r.body);
        const body =
          r.body === undefined
            ? ""
            : isObject
              ? JSON.stringify(r.body)
              : (r.body as string | Buffer);
        res.writeHead(r.status ?? 200, {
          ...(isObject ? { "content-type": "application/json" } : {}),
          ...r.headers,
        });
        res.end(body);
      };
      if (r.delayMs !== undefined) setTimeout(finish, r.delayMs);
      else finish();
    });
  });
  server.on("connection", (s) => {
    sockets.add(s);
    s.on("close", () => sockets.delete(s));
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = (server.address() as AddressInfo).port;
  return {
    url: `http://127.0.0.1:${port}`,
    port,
    requests,
    close: () =>
      new Promise<void>((resolve) => {
        for (const s of sockets) s.destroy();
        server.close(() => resolve());
      }),
  };
}

interface Fixture {
  response: {
    status?: number;
    headers?: Record<string, string>;
    body?: { json?: unknown; text?: string; base64?: string };
    bodyExcerpt?: string[];
  };
}

/** Loads `test/fixtures/<name>.json` (recorded against the real 0.165.0 collector). */
export function fixture(name: string): Fixture {
  return JSON.parse(
    readFileSync(new URL(`../fixtures/${name}.json`, import.meta.url), "utf8"),
  ) as Fixture;
}

/** The fixture's response as a mock response. */
export function fixtureResponse(name: string): RawResponse {
  const { response } = fixture(name);
  const body = response.body;
  const out: RawResponse = { status: response.status ?? 200 };
  const ct = response.headers?.["Content-Type"];
  if (ct !== undefined) out.headers = { "content-type": ct };
  if (body?.json !== undefined) out.body = JSON.stringify(body.json);
  else if (body?.text !== undefined) out.body = body.text;
  else if (body?.base64 !== undefined) out.body = Buffer.from(body.base64, "base64");
  else if (response.bodyExcerpt !== undefined) out.body = response.bodyExcerpt.join("\n") + "\n";
  return out;
}
