"""Query clients of the collector. The collector stores nothing and has no query API, so every
method of the SPEC §4/§5 query clients raises :class:`UnsupportedError` (read traces and logs
from the backend the collector exports to). Service-owned module.
"""

from __future__ import annotations

from collections.abc import Callable
from datetime import datetime, timedelta
from typing import Any, NoReturn

from ._core import UnsupportedError
from ._logging import LogRecord, LogSearchResult
from ._tracing import DependencyLink, Trace, TraceSearchQuery, TraceSummary

__all__ = [
    "AsyncCollectorLogQueryClient",
    "AsyncCollectorTraceQueryClient",
    "CollectorLogQueryClient",
    "CollectorTraceQueryClient",
]


def _unsupported(category: str, operation: str) -> NoReturn:
    raise UnsupportedError(
        f"{category}.{operation}: the OpenTelemetry Collector has no query API "
        "(read data from the backend it exports to)"
    )


class CollectorTraceQueryClient:
    """``TraceQueryClient`` of the collector: every method raises :class:`UnsupportedError`."""

    def get_trace(self, trace_id: str) -> Trace | None:
        """Always :class:`UnsupportedError`."""
        _unsupported("traces", "get_trace")

    def search_traces(self, query: TraceSearchQuery | None = None) -> list[TraceSummary]:
        """Always :class:`UnsupportedError`."""
        _unsupported("traces", "search_traces")

    def list_services(self) -> list[str]:
        """Always :class:`UnsupportedError`."""
        _unsupported("traces", "list_services")

    def list_operations(self, service: str) -> list[str]:
        """Always :class:`UnsupportedError`."""
        _unsupported("traces", "list_operations")

    def get_dependencies(
        self, *, start: datetime | None = None, end: datetime | None = None
    ) -> list[DependencyLink]:
        """Always :class:`UnsupportedError`."""
        _unsupported("traces", "get_dependencies")

    def wait_for_trace(self, trace_id: str, **_: Any) -> Trace:
        """Always :class:`UnsupportedError`."""
        _unsupported("traces", "wait_for_trace")


class AsyncCollectorTraceQueryClient:
    """Async ``TraceQueryClient`` of the collector: every method raises
    :class:`UnsupportedError`."""

    async def get_trace(self, trace_id: str) -> Trace | None:
        """Always :class:`UnsupportedError`."""
        _unsupported("traces", "get_trace")

    async def search_traces(self, query: TraceSearchQuery | None = None) -> list[TraceSummary]:
        """Always :class:`UnsupportedError`."""
        _unsupported("traces", "search_traces")

    async def list_services(self) -> list[str]:
        """Always :class:`UnsupportedError`."""
        _unsupported("traces", "list_services")

    async def list_operations(self, service: str) -> list[str]:
        """Always :class:`UnsupportedError`."""
        _unsupported("traces", "list_operations")

    async def get_dependencies(
        self, *, start: datetime | None = None, end: datetime | None = None
    ) -> list[DependencyLink]:
        """Always :class:`UnsupportedError`."""
        _unsupported("traces", "get_dependencies")

    async def wait_for_trace(self, trace_id: str, **_: Any) -> Trace:
        """Always :class:`UnsupportedError`."""
        _unsupported("traces", "wait_for_trace")


class CollectorLogQueryClient:
    """``LogQueryClient`` of the collector: every method raises :class:`UnsupportedError`."""

    def search(self, query: str, **_: Any) -> LogSearchResult:
        """Always :class:`UnsupportedError`."""
        _unsupported("logs", "search")

    def tail(
        self, query: str, on_record: Callable[[LogRecord], None], *, interval_s: float = 1.0
    ) -> Any:
        """Always :class:`UnsupportedError`."""
        _unsupported("logs", "tail")

    def list_streams(self) -> list[str]:
        """Always :class:`UnsupportedError`."""
        _unsupported("logs", "list_streams")

    def list_labels(self) -> list[str]:
        """Always :class:`UnsupportedError`."""
        _unsupported("logs", "list_labels")

    def wait_for_log(
        self, query: str, *, timeout_s: float | timedelta = 30.0, **_: Any
    ) -> LogRecord:
        """Always :class:`UnsupportedError`."""
        _unsupported("logs", "wait_for_log")


class AsyncCollectorLogQueryClient:
    """Async ``LogQueryClient`` of the collector: every method raises
    :class:`UnsupportedError`."""

    async def search(self, query: str, **_: Any) -> LogSearchResult:
        """Always :class:`UnsupportedError`."""
        _unsupported("logs", "search")

    async def tail(
        self, query: str, on_record: Callable[[LogRecord], None], *, interval_s: float = 1.0
    ) -> Any:
        """Always :class:`UnsupportedError`."""
        _unsupported("logs", "tail")

    async def list_streams(self) -> list[str]:
        """Always :class:`UnsupportedError`."""
        _unsupported("logs", "list_streams")

    async def list_labels(self) -> list[str]:
        """Always :class:`UnsupportedError`."""
        _unsupported("logs", "list_labels")

    async def wait_for_log(
        self, query: str, *, timeout_s: float | timedelta = 30.0, **_: Any
    ) -> LogRecord:
        """Always :class:`UnsupportedError`."""
        _unsupported("logs", "wait_for_log")
