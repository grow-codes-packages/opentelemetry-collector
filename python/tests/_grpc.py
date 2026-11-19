"""An in-process gRPC server speaking the OTLP export methods with raw bytes (unit tests)."""

from __future__ import annotations

import dataclasses
import time
from collections import defaultdict, deque
from concurrent.futures import ThreadPoolExecutor
from typing import Any

import grpc


@dataclasses.dataclass
class GrpcReply:
    """``body`` bytes for success, or ``code`` + ``details`` for an error status."""

    body: bytes = b"\n\x00"
    code: grpc.StatusCode | None = None
    details: str = ""
    delay: float = 0.0


@dataclasses.dataclass
class GrpcSeen:
    method: str
    body: bytes
    metadata: dict[str, str]


SERVICES = {
    "opentelemetry.proto.collector.trace.v1.TraceService": "traces",
    "opentelemetry.proto.collector.logs.v1.LogsService": "logs",
    "opentelemetry.proto.collector.metrics.v1.MetricsService": "metrics",
}


class GrpcServer:
    """Queue replies per signal (``traces``/``logs``/``metrics``) and inspect requests."""

    def __init__(self) -> None:
        self.replies: dict[str, deque[GrpcReply]] = defaultdict(deque)
        self.seen: list[GrpcSeen] = []
        self.server = grpc.server(ThreadPoolExecutor(max_workers=4))
        handlers = []
        for service, signal in SERVICES.items():
            handlers.append(
                grpc.method_handlers_generic_handler(
                    service, {"Export": grpc.unary_unary_rpc_method_handler(self._make(signal))}
                )
            )
        self.server.add_generic_rpc_handlers(tuple(handlers))
        self.port = self.server.add_insecure_port("127.0.0.1:0")
        self.server.start()

    def _make(self, signal: str) -> Any:
        def handle(request: bytes, context: grpc.ServicerContext) -> bytes:
            meta = {k: str(v) for k, v in context.invocation_metadata()}
            self.seen.append(GrpcSeen(signal, request, meta))
            queue = self.replies.get(signal)
            reply = queue.popleft() if queue else GrpcReply()
            if reply.delay:
                time.sleep(reply.delay)
            if reply.code is not None:
                context.abort(reply.code, reply.details)
            return reply.body

        return handle

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.port}"

    def enqueue(self, signal: str, *replies: GrpcReply) -> None:
        self.replies[signal].extend(replies)

    def stop(self) -> None:
        self.server.stop(None)
