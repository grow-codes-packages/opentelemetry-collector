# TypeScript example

Checks the collector (health with zpages fallback, ping, version), then sends a trace, a correlated
log record and metrics (counter, histogram, gauge) with `initTelemetry`.

```sh
(cd ../../typescript && npm install && npm run build)
npm install
GROW_OPENTELEMETRY_COLLECTOR_URL=http://127.0.0.1:4318 OTEL_SERVICE_NAME=checkout npm start
# gRPC instead of http/protobuf:
OTEL_EXPORTER_OTLP_PROTOCOL=grpc OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4317 npm start
```

Replace the `file:` dependency in `package.json` with a version (`npm install @grow-codes/opentelemetry-collector`)
to run it against the published package.
