# Java examples

Install the package into your local Maven repository, point it at a running OpenTelemetry Collector and run an
example:

```bash
make -C ../../java pack            # builds and installs codes.grow.packages:opentelemetry-collector
export GROW_OPENTELEMETRY_COLLECTOR_URL=http://127.0.0.1:4318
export OTEL_SERVICE_NAME=java-example
export OTEL_EXPORTER_OTLP_HEADERS="Authorization=Bearer%20..."   # only behind an authenticating gateway
mvn -q compile exec:java                                                  # BasicExample
mvn -q compile exec:java -Dexec.mainClass=codes.grow.packages.otelcollector.examples.AsyncExample
```

Against the published package, set `-Dsdk.version=<version>` (or edit `pom.xml`) instead of
installing it locally.

| Class | Shows |
| - | - |
| `BasicExample` | health, ping, server version, `initTelemetry` with a span, a counter, a histogram and a correlated log |
| `AsyncExample` | `…Async` methods, combining futures, cancellation |
| `HooksAndLoggingExample` | request hooks, a logger, retries and timeouts |
