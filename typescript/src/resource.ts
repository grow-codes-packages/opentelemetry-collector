// Service-owned: best-effort client-side resource detection (host, process, container,
// Kubernetes). Synchronous and side-effect free apart from reading a few /proc files.

import { readFileSync } from "node:fs";
import * as os from "node:os";
import { basename } from "node:path";
import { RESOURCE_DETECTORS, type ResourceDetectorName } from "./config.js";
import type { EnvMap } from "./core/index.js";
import type { SpanAttributes } from "./tracing/index.js";

/** Test seams of {@link detectResourceAttributes}. */
export interface DetectResourceOptions {
  /** Detectors to run. Default: all. */
  detectors?: readonly ResourceDetectorName[];
  /** Environment (Kubernetes downward-API variables). Default `process.env`. */
  env?: EnvMap;
  /** Reads a text file; returns `undefined` when it cannot. Default: `fs.readFileSync`. */
  readFile?: (path: string) => string | undefined;
}

const CONTAINER_ID = /([0-9a-f]{64})/;
const K8S_NAMESPACE_FILE = "/var/run/secrets/kubernetes.io/serviceaccount/namespace";

function defaultReadFile(path: string): string | undefined {
  try {
    return readFileSync(path, "utf8");
  } catch {
    return undefined;
  }
}

const ARCH: Record<string, string> = {
  x64: "amd64",
  arm64: "arm64",
  arm: "arm32",
  ia32: "x86",
  ppc64: "ppc64",
  s390x: "s390x",
};

function hostAttributes(): SpanAttributes {
  const type = os.type().toLowerCase();
  return {
    "host.name": os.hostname(),
    "host.arch": ARCH[process.arch] ?? process.arch,
    "os.type": type.startsWith("windows") ? "windows" : type,
    "os.version": os.release(),
  };
}

function processAttributes(): SpanAttributes {
  const out: SpanAttributes = {
    "process.pid": process.pid,
    "process.executable.name": basename(process.execPath),
    "process.executable.path": process.execPath,
    "process.runtime.name": "nodejs",
    "process.runtime.version": process.versions.node,
    "process.runtime.description": "Node.js",
  };
  const script = process.argv[1];
  if (script !== undefined) out["process.command"] = script;
  try {
    out["process.owner"] = os.userInfo().username;
  } catch {
    // no passwd entry (scratch containers)
  }
  return out;
}

/** Container id from `/proc/self/cgroup` (v1) or `/proc/self/mountinfo` (v2); `undefined` if none. */
export function containerId(readFile: (path: string) => string | undefined): string | undefined {
  const cgroup = readFile("/proc/self/cgroup");
  if (cgroup !== undefined) {
    for (const line of cgroup.split("\n")) {
      const id = CONTAINER_ID.exec(line.slice(line.lastIndexOf("/") + 1))?.[1];
      if (id !== undefined) return id;
    }
  }
  const mountinfo = readFile("/proc/self/mountinfo");
  if (mountinfo !== undefined) {
    for (const line of mountinfo.split("\n")) {
      if (!/\/(containers|sandboxes)\//.test(line) || !line.includes("hostname")) continue;
      const id = /\/(?:containers|sandboxes)\/([0-9a-f]{64})\//.exec(line)?.[1];
      if (id !== undefined) return id;
    }
  }
  return undefined;
}

function k8sAttributes(
  env: EnvMap,
  readFile: (path: string) => string | undefined,
): SpanAttributes {
  const pick = (...names: string[]): string | undefined => {
    for (const n of names) {
      const v = env[n]?.trim();
      if (v !== undefined && v !== "") return v;
    }
    return undefined;
  };
  const out: SpanAttributes = {};
  const inCluster = pick("KUBERNETES_SERVICE_HOST") !== undefined;
  const set = (key: string, value: string | undefined): void => {
    if (value !== undefined) out[key] = value;
  };
  set(
    "k8s.pod.name",
    pick("K8S_POD_NAME", "POD_NAME") ?? (inCluster ? pick("HOSTNAME") : undefined),
  );
  set(
    "k8s.namespace.name",
    pick("K8S_NAMESPACE_NAME", "POD_NAMESPACE") ??
      (inCluster ? readFile(K8S_NAMESPACE_FILE)?.trim() || undefined : undefined),
  );
  set("k8s.pod.uid", pick("K8S_POD_UID", "POD_UID"));
  set("k8s.node.name", pick("K8S_NODE_NAME", "NODE_NAME"));
  set("k8s.container.name", pick("K8S_CONTAINER_NAME", "CONTAINER_NAME"));
  set("k8s.deployment.name", pick("K8S_DEPLOYMENT_NAME"));
  set("k8s.cluster.name", pick("K8S_CLUSTER_NAME"));
  return out;
}

/**
 * Detects resource attributes: `host` (`host.name`, `host.arch`, `os.type`, `os.version`),
 * `process` (`process.pid`, `process.executable.*`, `process.runtime.*`, `process.command`,
 * `process.owner`), `container` (`container.id` from cgroups) and `k8s` (`k8s.pod.name`,
 * `k8s.namespace.name`, … from the downward-API variables `K8S_*`/`POD_*`, `HOSTNAME` and the
 * service-account namespace file). Detectors that find nothing add nothing.
 */
export function detectResourceAttributes(options: DetectResourceOptions = {}): SpanAttributes {
  const detectors = new Set(options.detectors ?? RESOURCE_DETECTORS);
  const env = options.env ?? process.env;
  const readFile = options.readFile ?? defaultReadFile;
  const out: SpanAttributes = {};
  if (detectors.has("host")) Object.assign(out, hostAttributes());
  if (detectors.has("process")) Object.assign(out, processAttributes());
  if (detectors.has("container")) {
    const id = containerId(readFile);
    if (id !== undefined) out["container.id"] = id;
  }
  if (detectors.has("k8s")) Object.assign(out, k8sAttributes(env, readFile));
  return out;
}
