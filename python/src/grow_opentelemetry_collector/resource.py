"""Best-effort client-side resource detection (host, process, container, Kubernetes).

Synchronous and side-effect free apart from reading a few ``/proc`` files. Service-owned module.
"""

from __future__ import annotations

import contextlib
import getpass
import os
import platform
import re
import socket
import sys
from collections.abc import Callable, Iterable, Mapping
from typing import Any

from .config import RESOURCE_DETECTORS

__all__ = ["container_id", "detect_resource_attributes"]

_CONTAINER_ID = re.compile(r"([0-9a-f]{64})")
_MOUNT_ID = re.compile(r"/(?:containers|sandboxes)/([0-9a-f]{64})/")
_K8S_NAMESPACE_FILE = "/var/run/secrets/kubernetes.io/serviceaccount/namespace"
_ARCH = {
    "x86_64": "amd64",
    "amd64": "amd64",
    "aarch64": "arm64",
    "arm64": "arm64",
    "armv7l": "arm32",
    "i386": "x86",
    "i686": "x86",
    "ppc64le": "ppc64",
    "s390x": "s390x",
}

ReadFile = Callable[[str], "str | None"]


def _read_file(path: str) -> str | None:
    try:
        with open(path, encoding="utf-8") as fh:
            return fh.read()
    except OSError:
        return None


def _host() -> dict[str, Any]:
    machine = platform.machine().lower()
    return {
        "host.name": socket.gethostname(),
        "host.arch": _ARCH.get(machine, machine),
        "os.type": platform.system().lower(),
        "os.version": platform.release(),
    }


def _process() -> dict[str, Any]:
    out: dict[str, Any] = {
        "process.pid": os.getpid(),
        "process.executable.name": os.path.basename(sys.executable or "python"),
        "process.executable.path": sys.executable,
        "process.runtime.name": sys.implementation.name,
        "process.runtime.version": platform.python_version(),
        "process.runtime.description": sys.version.replace("\n", " "),
    }
    if sys.argv and sys.argv[0]:
        out["process.command"] = sys.argv[0]
    with contextlib.suppress(Exception):  # no passwd entry (scratch containers)
        out["process.owner"] = getpass.getuser()
    return out


def container_id(read_file: ReadFile = _read_file) -> str | None:
    """Container id from ``/proc/self/cgroup`` (v1) or ``/proc/self/mountinfo`` (v2);
    ``None`` outside a container."""
    cgroup = read_file("/proc/self/cgroup")
    if cgroup:
        for line in cgroup.splitlines():
            match = _CONTAINER_ID.search(line.rsplit("/", 1)[-1])
            if match:
                return match.group(1)
    mountinfo = read_file("/proc/self/mountinfo")
    if mountinfo:
        for line in mountinfo.splitlines():
            if "hostname" not in line:
                continue
            match = _MOUNT_ID.search(line)
            if match:
                return match.group(1)
    return None


def _k8s(env: Mapping[str, str], read_file: ReadFile) -> dict[str, Any]:
    def pick(*names: str) -> str | None:
        for name in names:
            value = (env.get(name) or "").strip()
            if value:
                return value
        return None

    in_cluster = pick("KUBERNETES_SERVICE_HOST") is not None
    out: dict[str, Any] = {}

    def put(key: str, value: str | None) -> None:
        if value:
            out[key] = value

    pod = pick("K8S_POD_NAME", "POD_NAME") or (pick("HOSTNAME") if in_cluster else None)
    put("k8s.pod.name", pod)
    namespace = pick("K8S_NAMESPACE_NAME", "POD_NAMESPACE")
    if namespace is None and in_cluster:
        namespace = (read_file(_K8S_NAMESPACE_FILE) or "").strip() or None
    put("k8s.namespace.name", namespace)
    put("k8s.pod.uid", pick("K8S_POD_UID", "POD_UID"))
    put("k8s.node.name", pick("K8S_NODE_NAME", "NODE_NAME"))
    put("k8s.container.name", pick("K8S_CONTAINER_NAME", "CONTAINER_NAME"))
    put("k8s.deployment.name", pick("K8S_DEPLOYMENT_NAME"))
    put("k8s.cluster.name", pick("K8S_CLUSTER_NAME"))
    return out


def detect_resource_attributes(
    detectors: Iterable[str] | None = None,
    *,
    env: Mapping[str, str] | None = None,
    read_file: ReadFile | None = None,
) -> dict[str, Any]:
    """Detect resource attributes.

    ``host``: ``host.name``, ``host.arch``, ``os.type``, ``os.version``; ``process``:
    ``process.pid``, ``process.executable.*``, ``process.runtime.*``, ``process.command``,
    ``process.owner``; ``container``: ``container.id`` from cgroups; ``k8s``:
    ``k8s.pod.name``, ``k8s.namespace.name`` … from the downward-API variables
    (``K8S_*``/``POD_*``), ``HOSTNAME`` and the service-account namespace file. Detectors that
    find nothing add nothing. ``detectors`` defaults to all of them.
    """
    wanted = set(RESOURCE_DETECTORS if detectors is None else detectors)
    source = os.environ if env is None else env
    reader = read_file or _read_file
    out: dict[str, Any] = {}
    if "host" in wanted:
        out.update(_host())
    if "process" in wanted:
        out.update(_process())
    if "container" in wanted:
        cid = container_id(reader)
        if cid:
            out["container.id"] = cid
    if "k8s" in wanted:
        out.update(_k8s(source, reader))
    return out
