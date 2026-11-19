import { describe, expect, it } from "vitest";
import { containerId, detectResourceAttributes } from "../../src/index.js";

const ID = "a".repeat(64);

describe("detectResourceAttributes", () => {
  it("detects host and process attributes", () => {
    const attrs = detectResourceAttributes({
      detectors: ["host", "process"],
      readFile: () => undefined,
    });
    expect(typeof attrs["host.name"]).toBe("string");
    expect(attrs["host.arch"]).toBeDefined();
    expect(attrs["os.type"]).toBeDefined();
    expect(attrs["process.pid"]).toBe(process.pid);
    expect(attrs["process.runtime.name"]).toBe("nodejs");
    expect(attrs["process.runtime.version"]).toBe(process.versions.node);
    expect(attrs["container.id"]).toBeUndefined();
  });

  it("finds the container id in cgroup v1 and mountinfo (v2)", () => {
    expect(
      containerId((p) => (p === "/proc/self/cgroup" ? `12:cpu:/docker/${ID}\n` : undefined)),
    ).toBe(ID);
    const mountinfo = `1 2 0:1 /var/lib/docker/containers/${ID}/hostname /etc/hostname rw\n`;
    expect(
      containerId((p) =>
        p === "/proc/self/cgroup"
          ? "0::/\n"
          : p === "/proc/self/mountinfo"
            ? `x\n${mountinfo}`
            : undefined,
      ),
    ).toBe(ID);
    expect(containerId(() => "0::/\n/containers/short/hostname\n")).toBeUndefined();
    expect(containerId(() => undefined)).toBeUndefined();
    const attrs = detectResourceAttributes({
      detectors: ["container"],
      readFile: (p) => (p === "/proc/self/cgroup" ? `0::/kubepods/${ID}` : undefined),
    });
    expect(attrs).toEqual({ "container.id": ID });
  });

  it("reads Kubernetes downward-API variables and the namespace file", () => {
    const attrs = detectResourceAttributes({
      detectors: ["k8s"],
      env: {
        KUBERNETES_SERVICE_HOST: "10.0.0.1",
        HOSTNAME: "pod-abc",
        K8S_NODE_NAME: "node-1",
        POD_UID: "uid-1",
        K8S_CONTAINER_NAME: "app",
        K8S_DEPLOYMENT_NAME: "dep",
        K8S_CLUSTER_NAME: "c1",
      },
      readFile: (p) => (p.endsWith("/namespace") ? "grow\n" : undefined),
    });
    expect(attrs).toEqual({
      "k8s.pod.name": "pod-abc",
      "k8s.namespace.name": "grow",
      "k8s.pod.uid": "uid-1",
      "k8s.node.name": "node-1",
      "k8s.container.name": "app",
      "k8s.deployment.name": "dep",
      "k8s.cluster.name": "c1",
    });
    expect(detectResourceAttributes({ detectors: ["k8s"], env: { HOSTNAME: "h" } })).toEqual({});
    expect(
      detectResourceAttributes({
        detectors: ["k8s"],
        env: { K8S_POD_NAME: "p", POD_NAMESPACE: "ns" },
      }),
    ).toEqual({ "k8s.pod.name": "p", "k8s.namespace.name": "ns" });
    expect(
      detectResourceAttributes({
        detectors: ["k8s"],
        env: { KUBERNETES_SERVICE_HOST: "x", POD_NAME: "  " },
        readFile: () => "  ",
      }),
    ).toEqual({});
    expect(Object.keys(detectResourceAttributes()).length).toBeGreaterThan(5);
  });
});
