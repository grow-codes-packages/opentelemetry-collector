"""Grow Codes SDK for the OpenTelemetry Collector.

One-call OpenTelemetry bootstrap (traces, logs, metrics over OTLP), a health/version client and
OTLP log exporters for the collector every Grow stack runs.

>>> from grow_opentelemetry_collector import init_telemetry, OtelCollectorClient
>>> handle = init_telemetry()  # doctest: +SKIP
>>> client = OtelCollectorClient.from_env()  # doctest: +SKIP
"""

from . import _core
from ._core import *  # noqa: F403
from ._version import __version__
from .client import AsyncOtelCollectorClient, OtelCollectorClient
from .config import (
    ALIAS_ENV_PREFIX,
    DEFAULT_EXPORT_TIMEOUT_S,
    DEFAULT_GRPC_URL,
    DEFAULT_METRIC_EXPORT_INTERVAL_S,
    DEFAULT_URL,
    RESOURCE_DETECTORS,
    SIGNALS,
    VENDOR_ENV,
    OtelCollectorConfig,
    OtlpProtocol,
    ResourceDetectorName,
    Signal,
    parse_key_value_list,
)

__all__ = [
    "__version__",
    "OtelCollectorClient",
    "AsyncOtelCollectorClient",
    "OtelCollectorConfig",
    "ALIAS_ENV_PREFIX",
    "DEFAULT_EXPORT_TIMEOUT_S",
    "DEFAULT_GRPC_URL",
    "DEFAULT_METRIC_EXPORT_INTERVAL_S",
    "DEFAULT_URL",
    "RESOURCE_DETECTORS",
    "SIGNALS",
    "VENDOR_ENV",
    "OtlpProtocol",
    "ResourceDetectorName",
    "Signal",
    "parse_key_value_list",
]
__all__ += _core.__all__

from . import _logging  # noqa: E402
from ._logging import *  # noqa: E402, F403

__all__ += _logging.__all__

from . import _tracing  # noqa: E402
from ._tracing import *  # noqa: E402, F403

__all__ += _tracing.__all__

from . import health, logs, metrics, otlp, query, resource, setup, telemetry  # noqa: E402
from .health import *  # noqa: E402, F403
from .logs import *  # noqa: E402, F403
from .metrics import *  # noqa: E402, F403
from .otlp import *  # noqa: E402, F403
from .query import *  # noqa: E402, F403
from .resource import *  # noqa: E402, F403
from .setup import *  # noqa: E402, F403
from .telemetry import *  # noqa: E402, F403

for _module in (health, logs, metrics, otlp, query, resource, setup, telemetry):
    __all__ += _module.__all__
del _module
