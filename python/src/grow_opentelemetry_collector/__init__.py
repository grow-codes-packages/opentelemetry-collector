"""Grow Codes SDK for OpenTelemetry Collector.

>>> from grow_opentelemetry_collector import OtelCollectorClient, OtelCollectorConfig
>>> client = OtelCollectorClient.from_env()  # doctest: +SKIP
"""

from . import _core
from ._core import *  # noqa: F403
from ._version import __version__
from .client import AsyncOtelCollectorClient, OtelCollectorClient
from .config import DEFAULT_URL, OtelCollectorConfig

__all__ = [
    "__version__",
    "OtelCollectorClient",
    "AsyncOtelCollectorClient",
    "OtelCollectorConfig",
    "DEFAULT_URL",
]
__all__ += _core.__all__

from . import _logging  # noqa: E402
from ._logging import *  # noqa: E402, F403

__all__ += _logging.__all__

from . import _tracing  # noqa: E402
from ._tracing import *  # noqa: E402, F403

__all__ += _tracing.__all__
