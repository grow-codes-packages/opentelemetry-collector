# TypeScript integration tests

A consumer project: it installs the packed `@grow-codes/opentelemetry-collector` tarball (never the source tree)
and runs the integration and conformance tests with `node --test` against a real OpenTelemetry Collector.

```sh
cd typescript && make pack            # writes dist/grow-codes-opentelemetry-collector-<version>.tgz
cd integration && npm run install-package
GROW_OPENTELEMETRY_COLLECTOR_URL=http://127.0.0.1:<port> npm test
```

In CI and locally with Docker this runs inside `docker/Dockerfile.typescript`
(`make test-docker LANGS=typescript` from the repository root), which reads `/bootstrap/env`.
