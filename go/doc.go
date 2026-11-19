// Package otelcollector is the Grow Codes SDK for the OpenTelemetry Collector: one call
// to send traces, metrics and logs over OTLP (gRPC or HTTP/protobuf) with the official
// OpenTelemetry SDK and exporters, plus a health client, a log client and the Grow
// tracing and logging helpers.
//
// Configure it from the environment (GROW_OPENTELEMETRY_COLLECTOR_*, GROW_OTEL_* and the
// standard OTEL_* variables such as OTEL_EXPORTER_OTLP_ENDPOINT and OTEL_SERVICE_NAME)
// or from a Config literal:
//
//	client, err := otelcollector.NewClientFromEnv()
//	if err != nil {
//		return err
//	}
//	defer client.Close()
//	telemetry, err := client.InitTelemetry(ctx) // traces, metrics, logs
//	if err != nil {
//		return err
//	}
//	ctx, span := telemetry.Tracer("app").Start(ctx, "work")
//	telemetry.Logger("app", nil).Info(ctx, "working")
//	span.End()
//
// The collector stores nothing, so TraceQuery and LogQuery return UNSUPPORTED errors;
// read telemetry back from the backend the collector exports to.
//
// Every error is an *Error; compare codes with errors.Is against the Err* sentinels
// (errors.Is(err, otelcollector.ErrAuthentication)). Header values such as
// Authorization never appear in errors, hooks or logs.
//
// OpenTelemetry is a trademark of The Linux Foundation (OpenTelemetry is a CNCF
// project). This package is not affiliated with or endorsed by The Linux Foundation.
package otelcollector
