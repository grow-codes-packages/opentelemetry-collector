"""Collector health (``health_check`` extension on :13133 with a zpages fallback on :55679) and
the admin reads used by ``server_version`` (zpages ``/debug/servicez``, internal telemetry
``target_info``). Admin endpoints are unauthenticated: export headers are never sent to them.

Service-owned module.
"""

from __future__ import annotations

import re
import threading
from collections.abc import Mapping
from datetime import timedelta
from types import TracebackType
from typing import Any

from ._core import (
    AsyncHttpTransport,
    AuthenticationError,
    BaseConfig,
    CancelledError,
    CancelToken,
    ConfigError,
    HealthStatus,
    HttpTransport,
    PermissionDeniedError,
    Request,
    Response,
    ServiceError,
    TimeoutError,
    UnavailableError,
    UnsupportedError,
    ValidationError,
    apoll_until,
    poll_until,
    to_seconds,
)
from .config import OtelCollectorConfig
from .otlp import split_url

__all__ = [
    "DEFAULT_READY_INTERVAL_S",
    "DEFAULT_READY_TIMEOUT_S",
    "AsyncCollectorHealthClient",
    "CollectorHealthClient",
    "parse_servicez_version",
    "parse_target_info_version",
    "transport_config",
]

#: Default deadline of ``wait_until_ready``, in seconds.
DEFAULT_READY_TIMEOUT_S = 30.0
#: Default pause between ``wait_until_ready`` probes, in seconds.
DEFAULT_READY_INTERVAL_S = 0.5

_VERSION_ROW = re.compile(r"<b>Version</b></td>\s*<td>[^<]*</td>\s*<td[^>]*>([^<]+)</td>")
_TARGET_INFO = re.compile(r"^target_info\{[^}]*\bservice_version=\"([^\"]+)\"", re.MULTILINE)
_SERVICEZ = "/debug/servicez"


def parse_servicez_version(html: str) -> str | None:
    """Collector version from the zpages ``/debug/servicez`` HTML (leading ``v`` removed);
    ``None`` when absent."""
    match = _VERSION_ROW.search(html)
    version = match.group(1).strip() if match else ""
    return version.removeprefix("v") or None


def parse_target_info_version(metrics: str) -> str | None:
    """``service_version`` of a Prometheus ``target_info`` series (leading ``v`` removed);
    ``None`` when absent."""
    match = _TARGET_INFO.search(metrics)
    return match.group(1).removeprefix("v") or None if match else None


def transport_config(
    cfg: OtelCollectorConfig, url: str, headers: Mapping[str, str] | None = None
) -> BaseConfig:
    """The transport settings of ``cfg`` for another base ``url``; ``headers`` default to none
    (admin endpoints are unauthenticated)."""
    return BaseConfig(
        url=url,
        connect_timeout=cfg.connect_timeout,
        request_timeout=cfg.request_timeout,
        total_timeout=cfg.total_timeout,
        retry=cfg.retry,
        tls=cfg.tls,
        proxy=cfg.proxy,
        headers=dict(headers or {}),
        user_agent_suffix=cfg.user_agent_suffix,
        hooks=cfg.hooks,
        logger=cfg.logger,
    )


def _stringify(details: Mapping[str, Any]) -> dict[str, str]:
    return {str(k): str(v) for k, v in details.items() if isinstance(v, (str, int, float, bool))}


def _health_from_response(resp: Response) -> HealthStatus:
    details: dict[str, str] = {"source": "health_check"}
    try:
        body: Any = resp.json()
    except ValidationError:
        body = None
    if isinstance(body, Mapping):
        rest = dict(body)
        vendor = rest.pop("status", None)
        if isinstance(vendor, str) and vendor:
            details["vendorStatus"] = vendor
        details.update(_stringify(rest))
    if resp.ok:
        return HealthStatus(True, "ok", details)
    details["httpStatus"] = str(resp.status)
    return HealthStatus(False, "unhealthy", details)


def _health_from_error(err: ServiceError) -> HealthStatus:
    if isinstance(err, (CancelledError, ConfigError)):
        raise err
    word = "unreachable" if isinstance(err, (UnavailableError, TimeoutError)) else "unhealthy"
    return HealthStatus(False, word, {"error": err.message})


_ZPAGES_OK = HealthStatus(True, "ok", {"source": "zpages"})
_FATAL_PROBE = (CancelledError, AuthenticationError, PermissionDeniedError)


def _no_version(cause: BaseException | None) -> UnsupportedError:
    return UnsupportedError(
        "server_version: the collector exposes no version "
        "(zpages and internal telemetry unavailable)",
        cause=cause,
    )


class _Targets:
    """Absolute admin URLs split into transport base and path."""

    def __init__(self, cfg: OtelCollectorConfig) -> None:
        self.health_base, self.health_path = split_url(str(cfg.health_url))
        zpages = str(cfg.zpages_url).rstrip("/")
        self.zpages_base, zpages_path = split_url(zpages)
        self.servicez_path = zpages_path.rstrip("/") + _SERVICEZ
        self.telemetry: tuple[str, str] | None = (
            split_url(cfg.telemetry_url) if cfg.telemetry_url else None
        )

    def health_request(self) -> Request:
        return Request(
            "GET", self.health_path, operation="health", raise_for_status=False, max_retries=0
        )

    def zpages_health_request(self) -> Request:
        return Request(
            "GET",
            self.servicez_path,
            operation="health",
            headers={"Accept": "text/html"},
            max_retries=0,
        )

    def servicez_request(self) -> Request:
        return Request(
            "GET", self.servicez_path, operation="server_version", headers={"Accept": "text/html"}
        )

    def telemetry_request(self, path: str) -> Request:
        return Request("GET", path, operation="server_version", headers={"Accept": "text/plain"})


class CollectorHealthClient:
    """Synchronous health client of an OpenTelemetry Collector.

    ``health()`` reads the ``health_check`` extension (``health_url``) and, when that endpoint is
    unreachable (stock Grow stacks bind it to localhost), falls back to zpages
    ``{zpages_url}/debug/servicez``. Every I/O method accepts ``cancel=``.
    """

    def __init__(
        self,
        config: OtelCollectorConfig | None = None,
        *,
        http_transport: Any = None,
    ) -> None:
        cfg = (config if config is not None else OtelCollectorConfig.from_env()).validate()
        self.config = cfg
        self._targets = _Targets(cfg)
        secrets = cfg.known_secrets

        def transport(base: str) -> HttpTransport:
            return HttpTransport(
                transport_config(cfg, base), known_secrets=secrets, http_transport=http_transport
            )

        self._health = transport(self._targets.health_base)
        self._zpages = transport(self._targets.zpages_base)
        self._telemetry = transport(self._targets.telemetry[0]) if self._targets.telemetry else None

    @classmethod
    def from_env(
        cls, env: Mapping[str, str] | None = None, **overrides: Any
    ) -> CollectorHealthClient:
        """Build from the environment (see :meth:`OtelCollectorConfig.from_env`)."""
        return cls(OtelCollectorConfig.from_env(env, **overrides))

    def health(self, *, cancel: CancelToken | None = None) -> HealthStatus:
        """Collector health; never raises for an unhealthy or unreachable collector.

        ``200 {"status":"Server available",…}`` → ``healthy=True, status="ok"`` with details
        ``source="health_check"``, ``vendorStatus``, ``upSince``, ``uptime``; ``503`` →
        ``unhealthy``; unreachable → zpages fallback (``details["source"] = "zpages"``), else
        ``unreachable``. Raises only :class:`CancelledError` and :class:`ConfigError`.
        """
        try:
            primary = _health_from_response(
                self._health.request(self._targets.health_request(), cancel=cancel)
            )
        except ServiceError as err:
            primary = _health_from_error(err)
        if primary.status != "unreachable":
            return primary
        try:
            self._zpages.request(self._targets.zpages_health_request(), cancel=cancel)
        except ServiceError as err:
            _health_from_error(err)
            return primary
        return _ZPAGES_OK

    def wait_until_ready(
        self,
        timeout_s: float | timedelta = DEFAULT_READY_TIMEOUT_S,
        *,
        interval_s: float | timedelta = DEFAULT_READY_INTERVAL_S,
        cancel: CancelToken | None = None,
    ) -> HealthStatus:
        """Poll :meth:`health` until healthy and return that status.

        Raises :class:`TimeoutError` after ``timeout_s`` and :class:`CancelledError` when
        ``cancel`` fires.
        """

        def sleep(seconds: float) -> None:
            if cancel is None:
                threading.Event().wait(seconds)
                return
            if cancel.wait(seconds):
                raise CancelledError("wait_until_ready: cancelled")

        return poll_until(
            lambda: self.health(cancel=cancel),
            timeout_s=to_seconds(timeout_s),
            interval_s=to_seconds(interval_s),
            predicate=lambda h: h.healthy,
            operation="wait_until_ready",
            sleep=sleep,
        )

    def server_version(self, *, cancel: CancelToken | None = None) -> str:
        """Collector version (``"0.165.0"``): zpages ``/debug/servicez`` Version row, else
        ``service_version`` of ``target_info`` at ``telemetry_url``.

        :class:`UnsupportedError` when neither answers with a version.
        """
        cause: BaseException | None = None
        try:
            resp = self._zpages.request(self._targets.servicez_request(), cancel=cancel)
            version = parse_servicez_version(resp.text)
            if version:
                return version
        except _FATAL_PROBE:
            raise
        except ServiceError as err:
            cause = err
        if self._telemetry is not None and self._targets.telemetry is not None:
            try:
                resp = self._telemetry.request(
                    self._targets.telemetry_request(self._targets.telemetry[1]), cancel=cancel
                )
                version = parse_target_info_version(resp.text)
                if version:
                    return version
            except _FATAL_PROBE:
                raise
            except ServiceError as err:
                cause = err
        raise _no_version(cause)

    def close(self) -> None:
        """Release connections (idempotent)."""
        for t in (self._health, self._zpages, self._telemetry):
            if t is not None:
                t.close()

    def __enter__(self) -> CollectorHealthClient:
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        self.close()

    def __repr__(self) -> str:
        return f"CollectorHealthClient(config={self.config!r})"


class AsyncCollectorHealthClient:
    """Asynchronous :class:`CollectorHealthClient`; cancel a call by cancelling its task."""

    def __init__(
        self,
        config: OtelCollectorConfig | None = None,
        *,
        http_transport: Any = None,
    ) -> None:
        cfg = (config if config is not None else OtelCollectorConfig.from_env()).validate()
        self.config = cfg
        self._targets = _Targets(cfg)
        secrets = cfg.known_secrets

        def transport(base: str) -> AsyncHttpTransport:
            return AsyncHttpTransport(
                transport_config(cfg, base), known_secrets=secrets, http_transport=http_transport
            )

        self._health = transport(self._targets.health_base)
        self._zpages = transport(self._targets.zpages_base)
        self._telemetry = transport(self._targets.telemetry[0]) if self._targets.telemetry else None

    @classmethod
    def from_env(
        cls, env: Mapping[str, str] | None = None, **overrides: Any
    ) -> AsyncCollectorHealthClient:
        """Build from the environment (see :meth:`OtelCollectorConfig.from_env`)."""
        return cls(OtelCollectorConfig.from_env(env, **overrides))

    async def health(self) -> HealthStatus:
        """See :meth:`CollectorHealthClient.health`."""
        try:
            primary = _health_from_response(
                await self._health.request(self._targets.health_request())
            )
        except ServiceError as err:
            primary = _health_from_error(err)
        if primary.status != "unreachable":
            return primary
        try:
            await self._zpages.request(self._targets.zpages_health_request())
        except ServiceError as err:
            _health_from_error(err)
            return primary
        return _ZPAGES_OK

    async def wait_until_ready(
        self,
        timeout_s: float | timedelta = DEFAULT_READY_TIMEOUT_S,
        *,
        interval_s: float | timedelta = DEFAULT_READY_INTERVAL_S,
    ) -> HealthStatus:
        """See :meth:`CollectorHealthClient.wait_until_ready`."""
        return await apoll_until(
            self.health,
            timeout_s=to_seconds(timeout_s),
            interval_s=to_seconds(interval_s),
            predicate=lambda h: h.healthy,
            operation="wait_until_ready",
        )

    async def server_version(self) -> str:
        """See :meth:`CollectorHealthClient.server_version`."""
        cause: BaseException | None = None
        try:
            resp = await self._zpages.request(self._targets.servicez_request())
            version = parse_servicez_version(resp.text)
            if version:
                return version
        except _FATAL_PROBE:
            raise
        except ServiceError as err:
            cause = err
        if self._telemetry is not None and self._targets.telemetry is not None:
            try:
                resp = await self._telemetry.request(
                    self._targets.telemetry_request(self._targets.telemetry[1])
                )
                version = parse_target_info_version(resp.text)
                if version:
                    return version
            except _FATAL_PROBE:
                raise
            except ServiceError as err:
                cause = err
        raise _no_version(cause)

    async def aclose(self) -> None:
        """Release connections (idempotent)."""
        for t in (self._health, self._zpages, self._telemetry):
            if t is not None:
                await t.aclose()

    close = aclose

    async def __aenter__(self) -> AsyncCollectorHealthClient:
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        await self.aclose()

    def __repr__(self) -> str:
        return f"AsyncCollectorHealthClient(config={self.config!r})"
