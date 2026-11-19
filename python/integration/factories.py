"""Factories the integration suite and the conformance runner use (owned by this repository).

The collector has no query API: the tracing and logging query clients are test-only read-back
adapters (Jaeger sidecar, file-exporter capture) from ``readback.py``. Declared capabilities:
core ``version``; tracing ``query``, ``services``; logging ``query`` (no tail, labels, streams
or dependencies — see docs/compatibility.md).
"""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any

from readback import CaptureLogReadback, JaegerReadback

import grow_opentelemetry_collector as sdk
from grow_opentelemetry_collector.testing import LoggingTarget, TracingTarget
from grow_opentelemetry_collector.testing.logging_conformance import batch_options_for

URL_VAR = "GROW_OPENTELEMETRY_COLLECTOR_URL"

#: Capabilities of the deployment under test for the core scenarios ("version" = serverVersion).
CORE_CAPABILITIES: set[str] = {"version"}


def core_client(env: Mapping[str, str], **overrides: Any) -> sdk.OtelCollectorClient:
    """A client configured from the test environment (URLs from /bootstrap/env)."""
    return sdk.OtelCollectorClient.from_env(env, **overrides)


def async_core_client(env: Mapping[str, str], **overrides: Any) -> sdk.AsyncOtelCollectorClient:
    return sdk.AsyncOtelCollectorClient.from_env(env, **overrides)


#: Capabilities for the logging scenarios: query, tail, streams, labels.
LOGGING_CAPABILITIES: set[str] = {"query"}


def logging_target(env: Mapping[str, str], scenario: Mapping[str, Any]) -> LoggingTarget:
    """OTLP log client + capture read-back for one logging scenario."""
    config = sdk.OtelCollectorConfig.from_env(env, service_name="conformance-otelcol-py")
    client = sdk.create_log_client(config, options=batch_options_for(scenario))
    return LoggingTarget(client, CaptureLogReadback(env))


#: Capabilities for the tracing scenarios: query, services, dependencies.
TRACING_CAPABILITIES: set[str] = {"query", "services"}


def tracing_target(env: Mapping[str, str], service_name: str) -> TracingTarget:
    """``init_tracing`` (not global) + Jaeger read-back for one tracing scenario."""
    handle = sdk.init_tracing(
        collector=sdk.OtelCollectorConfig.from_env(env),
        service_name=service_name,
        set_global=False,
        batch=sdk.SpanBatchOptions(scheduled_delay_s=0.2),
    )
    return TracingTarget(handle, JaegerReadback(env))
