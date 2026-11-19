"""Shared conformance scenarios (conformance/scenarios) for the tracing and logging categories
against the real collector. The query side is the test-only read-back (Jaeger sidecar, file
exporter capture); see factories.py."""

from __future__ import annotations

import time
from typing import Any

import pytest
from factories import LOGGING_CAPABILITIES, TRACING_CAPABILITIES, logging_target, tracing_target

import grow_opentelemetry_collector as sdk
from grow_opentelemetry_collector.testing import ConformanceRunner, assert_scenario, pytest_params


@pytest.mark.parametrize("scenario", pytest_params(categories=["tracing"]))
def test_tracing_conformance(env: dict[str, str], run_id: str, scenario: dict[str, Any]) -> None:
    service = f"conformance-otelcol-py-{run_id}-{time.time_ns()}"
    runner = ConformanceRunner(
        {"tracing": lambda ctx: tracing_target(env, service)},
        capabilities=TRACING_CAPABILITIES,
        service_name=service,
    )
    result = assert_scenario(runner, scenario)
    assert result.status == "passed"


@pytest.mark.parametrize("scenario", pytest_params(categories=["logging"]))
def test_logging_conformance(env: dict[str, str], scenario: dict[str, Any]) -> None:
    runner = ConformanceRunner(
        {"logging": lambda ctx: logging_target(env, ctx.scenario)},
        capabilities=LOGGING_CAPABILITIES,
    )
    result = assert_scenario(runner, scenario)
    assert result.status == "passed"


def test_query_adapters_are_test_only() -> None:
    assert not hasattr(sdk, "JaegerReadback")
    assert not hasattr(sdk, "CaptureLogReadback")
