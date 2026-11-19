"""Shared pytest fixtures."""

from __future__ import annotations

from collections.abc import Iterator

import pytest

from _grpc import GrpcServer
from _support import MockServer


@pytest.fixture
def server() -> Iterator[MockServer]:
    srv = MockServer()
    yield srv
    srv.stop()


@pytest.fixture
def grpc_server() -> Iterator[GrpcServer]:
    srv = GrpcServer()
    yield srv
    srv.stop()
