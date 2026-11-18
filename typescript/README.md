# @grow-codes/opentelemetry-collector

TypeScript SDK for [OpenTelemetry Collector](https://grow-codes-packages.github.io/opentelemetry-collector/) by Grow Codes.
Node.js 22.12 or newer, ESM and CommonJS, full type definitions.

```sh
npm install @grow-codes/opentelemetry-collector
```

```ts
import { OtelCollectorClient } from "@grow-codes/opentelemetry-collector";

// Reads GROW_OPENTELEMETRY_COLLECTOR_URL, GROW_OPENTELEMETRY_COLLECTOR_TOKEN, timeouts, TLS and proxy settings.
const client = OtelCollectorClient.fromEnv();
try {
  console.log(await client.health()); // { healthy: true, status: "ok", details: {} }
  console.log(`ping: ${(await client.ping()).toFixed(1)} ms`);
} finally {
  await client.close();
}
```

Every error is a `OtelCollectorError` subclass with a canonical `code` (`NOT_FOUND`,
`AUTHENTICATION`, `RATE_LIMITED`, …), `httpStatus`, `requestId` and `retryable`. Credentials
never appear in messages, logs, hook data or `toString()` output.

Test helpers (in-memory fakes and the conformance runner) live in
`@grow-codes/opentelemetry-collector/testing`.

Documentation: https://grow-codes-packages.github.io/opentelemetry-collector/ · Licence: Apache-2.0.
OpenTelemetry Collector is a trademark of The Linux Foundation (OpenTelemetry is a CNCF project); this package is not affiliated with or endorsed by it.
