// Command example sends traces, metrics and logs to an OpenTelemetry Collector with
// the Grow Codes Go SDK: configuration from the environment, health, ping, server
// version, one-call telemetry bootstrap and the LogClient.
//
//	GROW_OPENTELEMETRY_COLLECTOR_URL=http://127.0.0.1:4318 OTEL_SERVICE_NAME=example go run .
package main

import (
	"context"
	"errors"
	"fmt"
	"os"
	"time"

	otelcollector "github.com/grow-codes-packages/opentelemetry-collector/go"
)

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, "error:", err)
		os.Exit(1)
	}
}

func run() error {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	cfg, err := otelcollector.ConfigFromEnv()
	if err != nil {
		return err // CONFIG error naming the variable, never its value
	}
	fmt.Println("config:", cfg) // header values such as Authorization print as [REDACTED]

	client, err := otelcollector.NewClient(cfg)
	if err != nil {
		return err
	}
	defer func() { _ = client.Close() }() // also shuts down the telemetry handle below

	health, err := client.WaitUntilReady(ctx, 10*time.Second)
	if err != nil {
		return err
	}
	fmt.Printf("health: healthy=%t status=%s source=%s\n", health.Healthy, health.Status, health.Details["source"])

	latency, err := client.Ping(ctx)
	switch {
	case errors.Is(err, otelcollector.ErrAuthentication):
		return fmt.Errorf("the collector rejected the configured headers: %w", err)
	case err != nil:
		return err
	}
	fmt.Println("ping:", latency)

	if version, err := client.ServerVersion(ctx); err == nil {
		fmt.Println("collector version:", version)
	} else if !errors.Is(err, otelcollector.ErrUnsupported) {
		return err
	}

	// One call for traces, metrics and logs (OTLP, shared resource).
	telemetry, err := client.InitTelemetry(ctx)
	if err != nil {
		return err
	}

	requests, err := telemetry.Counter("example_requests", otelcollector.InstrumentOptions{Unit: "1"})
	if err != nil {
		return err
	}
	logger := telemetry.Logger("example", nil)

	_, err = otelcollector.WithSpan(ctx, "handle-request", func(ctx context.Context) (string, error) {
		requests.Add(ctx, 1)
		otelcollector.AddEvent(ctx, "work-done", map[string]any{"items": 3})
		logger.Info(ctx, "request handled", "items", 3) // carries trace_id/span_id of the span
		return "ok", nil
	}, otelcollector.WithTracer(telemetry.Tracer("example")))
	if err != nil {
		return err
	}

	if err := telemetry.ForceFlush(ctx); err != nil {
		return err
	}
	fmt.Println("sent one span, one metric point and one log record")
	return telemetry.Shutdown(ctx)
}
