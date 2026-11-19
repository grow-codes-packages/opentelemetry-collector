package integration

import (
	"context"
	"strings"
	"testing"
	"time"

	otelcollector "github.com/grow-codes-packages/opentelemetry-collector/go"
	"github.com/grow-codes-packages/opentelemetry-collector/go/testkit"
)

// capabilities the deployment under test offers (see conformance "requires"). The
// collector has no query API: "query" and "services" are served by the test-only
// read-back adapters (Jaeger sidecar, capture files); tail, labels, streams and
// dependencies are not declared (docs/compatibility.md).
var capabilities = []string{"version", "query", "services"}

// categories runs "core" plus this service's categories.
func categories() []string {
	return append([]string{"core"}, strings.Split("logging,tracing", ",")...)
}

// factories builds the conformance targets: the core client; tracing = InitTracing
// (OTLP to the collector, configured from the environment) + the Jaeger read-back;
// logging = Client.LogClient (OTLP logs) + the capture-file read-back.
func factories(t *testing.T) testkit.Factories {
	t.Helper()
	return testkit.Factories{
		"core": testkit.CoreFactory(func(context.Context) (testkit.CoreTarget, error) {
			return otelcollector.NewClientFromEnv()
		}),
		"tracing": testkit.TracingFactory(func(ctx context.Context, serviceName string) (testkit.TracingTarget, error) {
			h, err := otelcollector.InitTracing(ctx, otelcollector.TracingConfig{
				ServiceName:      serviceName,
				DisableSetGlobal: true,
				Batch:            otelcollector.TracingBatchOptions{ScheduledDelay: 100 * time.Millisecond},
			})
			if err != nil {
				return testkit.TracingTarget{}, err
			}
			return testkit.TracingTarget{Handle: h, Query: jaegerTraceQuery{}}, nil
		}),
		"logging": testkit.LoggingFactory(func(_ context.Context, opts otelcollector.BatchOptions) (testkit.LoggingTarget, error) {
			client, err := otelcollector.NewClientFromEnv()
			if err != nil {
				return testkit.LoggingTarget{}, err
			}
			t.Cleanup(func() { _ = client.Close() })
			lc, err := client.LogClient(otelcollector.LogClientOptions{Batch: opts})
			if err != nil {
				return testkit.LoggingTarget{}, err
			}
			return testkit.LoggingTarget{Client: lc, Query: captureLogQuery{}}, nil
		}),
	}
}
