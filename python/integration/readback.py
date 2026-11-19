"""Test-only read-back for the collector fixture (the collector itself has no query API):

- the file exporter output served by ``capture`` (OTLP JSON lines),
- the Jaeger sidecar (``/api/v3/traces/{id}``, ``/api/v3/services``).

The conformance query adapters live here, never in the shipped package (DECISIONS.md §7).
"""

from __future__ import annotations

import json
import time
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any, TypeVar

import httpx

import grow_opentelemetry_collector as sdk

T = TypeVar("T")
Json = dict[str, Any]

_KINDS = ("unspecified", "internal", "server", "client", "producer", "consumer")
_STATUS = ("unset", "ok", "error")


def any_value(v: Mapping[str, Any] | None) -> Any:
    """OTLP JSON ``AnyValue`` → plain value."""
    if not v:
        return ""
    if "stringValue" in v:
        return v["stringValue"]
    if "intValue" in v:
        return int(v["intValue"])
    if "doubleValue" in v:
        return float(v["doubleValue"])
    if "boolValue" in v:
        return bool(v["boolValue"])
    if "arrayValue" in v:
        return [any_value(x) for x in v["arrayValue"].get("values", [])]
    if "kvlistValue" in v:
        return attrs(v["kvlistValue"].get("values"))
    return ""


def attrs(items: list[Json] | None) -> dict[str, Any]:
    """OTLP JSON ``KeyValue`` list → mapping."""
    return {kv["key"]: any_value(kv.get("value")) for kv in items or []}


def capture_lines(env: Mapping[str, str], signal: str) -> list[Json]:
    """Every JSON line of ``capture/<signal>.jsonl``."""
    resp = httpx.get(f"{env['OTELCOL_TEST_CAPTURE_URL']}/{signal}.jsonl", timeout=10)
    if resp.status_code == 404:
        return []
    resp.raise_for_status()
    return [json.loads(line) for line in resp.text.splitlines() if line.strip()]


@dataclass
class CapturedLog:
    resource: dict[str, Any]
    scope: str
    record: Json
    body: str
    attributes: dict[str, Any]


def captured_logs(env: Mapping[str, str]) -> list[CapturedLog]:
    out: list[CapturedLog] = []
    for line in capture_lines(env, "logs"):
        for rl in line.get("resourceLogs", []):
            resource = attrs(rl.get("resource", {}).get("attributes"))
            for sl in rl.get("scopeLogs", []):
                scope = sl.get("scope", {}).get("name", "")
                for record in sl.get("logRecords", []):
                    out.append(
                        CapturedLog(
                            resource,
                            scope,
                            record,
                            str(any_value(record.get("body"))),
                            attrs(record.get("attributes")),
                        )
                    )
    return out


@dataclass
class CapturedMetric:
    resource: dict[str, Any]
    metric: Json


def captured_metrics(env: Mapping[str, str]) -> list[CapturedMetric]:
    out: list[CapturedMetric] = []
    for line in capture_lines(env, "metrics"):
        for rm in line.get("resourceMetrics", []):
            resource = attrs(rm.get("resource", {}).get("attributes"))
            for sm in rm.get("scopeMetrics", []):
                for metric in sm.get("metrics", []):
                    out.append(CapturedMetric(resource, metric))
    return out


def points_with(metric: Json, attr: str, value: str) -> list[Json]:
    """Data points of a metric whose attributes carry ``attr=value``."""
    body = metric.get("sum") or metric.get("gauge") or metric.get("histogram") or {}
    return [p for p in body.get("dataPoints", []) if attrs(p.get("attributes")).get(attr) == value]


def eventually(fn: Callable[[], T | None], timeout_s: float = 20.0) -> T:
    """Poll ``fn`` until it returns something other than ``None``."""
    deadline = time.monotonic() + timeout_s
    while True:
        value = fn()
        if value is not None:
            return value
        if time.monotonic() > deadline:
            raise AssertionError("eventually: timed out")
        time.sleep(0.25)


def _micros(nanos: Any) -> int:
    return int(str(nanos or "0")) // 1000


def trace_from_otlp(trace_id: str, resource_spans: list[Json]) -> sdk.Trace:
    """OTLP JSON ``resourceSpans`` → normalized :class:`Trace`."""
    spans: list[sdk.Span] = []
    for rs in resource_spans:
        resource = attrs(rs.get("resource", {}).get("attributes"))
        for ss in rs.get("scopeSpans", []):
            for s in ss.get("spans", []):
                start, end = _micros(s.get("startTimeUnixNano")), _micros(s.get("endTimeUnixNano"))
                status = s.get("status") or {}
                spans.append(
                    sdk.Span(
                        trace_id=s["traceId"],
                        span_id=s["spanId"],
                        parent_span_id=s.get("parentSpanId") or None,
                        name=s["name"],
                        kind=_KINDS[int(s.get("kind", 0))],  # type: ignore[arg-type]
                        service_name=str(resource.get("service.name", "")),
                        start_micros=start,
                        end_micros=end,
                        duration_micros=end - start,
                        status=sdk.SpanStatus(
                            _STATUS[int(status.get("code", 0))],  # type: ignore[arg-type]
                            status.get("message"),
                        ),
                        attributes=attrs(s.get("attributes")),
                        events=[
                            sdk.SpanEvent(
                                e["name"],
                                _micros(e.get("timeUnixNano")),
                                attrs(e.get("attributes")),
                            )
                            for e in s.get("events", [])
                        ],
                        links=[
                            sdk.SpanLink(l["traceId"], l["spanId"], attrs(l.get("attributes")))
                            for l in s.get("links", [])  # noqa: E741
                        ],
                    )
                )
    return sdk.Trace(trace_id, sdk.sort_spans(spans))


class JaegerReadback(sdk.BaseTraceQueryClient):
    """Jaeger v3 read-back used as the conformance ``TraceQueryClient`` (test only)."""

    def __init__(self, env: Mapping[str, str]) -> None:
        self.base = env["OTELCOL_TEST_JAEGER_URL"]

    def get_trace(self, trace_id: str) -> sdk.Trace | None:
        resp = httpx.get(f"{self.base}/api/v3/traces/{trace_id}", timeout=10)
        if resp.status_code == 404:
            return None
        resp.raise_for_status()
        return trace_from_otlp(trace_id, resp.json().get("result", {}).get("resourceSpans", []))

    def list_services(self) -> list[str]:
        resp = httpx.get(f"{self.base}/api/v3/services", timeout=10)
        resp.raise_for_status()
        return list(resp.json().get("services") or [])


def _to_record(c: CapturedLog) -> sdk.LogRecord:
    r = c.record
    return sdk.LogRecord(
        body=c.body,
        severity=sdk.Severity.parse(int(r.get("severityNumber", 9))),
        severity_text=str(r.get("severityText", "")),
        timestamp=datetime.fromtimestamp(_micros(r.get("timeUnixNano")) / 1e6, tz=timezone.utc),
        attributes=c.attributes,
        resource=c.resource,
        trace_id=r.get("traceId") or None,
        span_id=r.get("spanId") or None,
    )


class CaptureLogReadback(sdk.BaseLogQueryClient):
    """Capture-file read-back used as the conformance ``LogQueryClient`` (substring match on the
    body; one page holds every match)."""

    def __init__(self, env: Mapping[str, str]) -> None:
        self.env = env

    def search(self, query: str, **_: Any) -> sdk.LogSearchResult:  # type: ignore[override]
        records = [_to_record(c) for c in captured_logs(self.env) if query in c.body]
        return sdk.LogSearchResult(records=records, total=len(records))
