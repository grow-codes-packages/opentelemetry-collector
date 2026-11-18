// Package otelcollector is the Grow Codes SDK for OpenTelemetry Collector.
//
// Create a client from the environment (GROW_OPENTELEMETRY_COLLECTOR_URL,
// GROW_OPENTELEMETRY_COLLECTOR_TOKEN, ...) or from a Config literal:
//
//	client, err := otelcollector.NewClientFromEnv()
//	if err != nil {
//		return err
//	}
//	defer client.Close()
//	status, err := client.Health(ctx)
//
// Every error is an *Error; compare codes with errors.Is against the Err*
// sentinels (errors.Is(err, otelcollector.ErrNotFound)). Credentials are wrapped in
// Secret and never appear in errors, hooks or logs.
//
// OpenTelemetry Collector is a trademark of The Linux Foundation (OpenTelemetry is a CNCF project). This package is not affiliated
// with or endorsed by The Linux Foundation (OpenTelemetry is a CNCF project).
package otelcollector
