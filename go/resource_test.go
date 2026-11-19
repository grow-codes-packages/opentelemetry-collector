package otelcollector

import (
	"context"
	"os"
	"path/filepath"
	"testing"

	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/sdk/resource"
)

func withEnv(t *testing.T, env map[string]string, nsFile string) {
	t.Helper()
	saved, savedFile := lookupEnv, k8sNamespaceFile
	t.Cleanup(func() { lookupEnv, k8sNamespaceFile = saved, savedFile })
	lookupEnv = func(k string) (string, bool) { v, ok := env[k]; return v, ok }
	k8sNamespaceFile = nsFile
}

func value(res *resource.Resource, key string) string {
	v, _ := res.Set().Value(attribute.Key(key))
	return v.String()
}

func TestKubernetesDetector(t *testing.T) {
	dir := t.TempDir()
	ns := filepath.Join(dir, "namespace")
	if err := os.WriteFile(ns, []byte("payments\n"), 0o600); err != nil {
		t.Fatal(err)
	}

	withEnv(t, map[string]string{"KUBERNETES_SERVICE_HOST": "10.0.0.1", "HOSTNAME": "pod-abc", "NODE_NAME": "node-1",
		"POD_UID": "uid-1", "CONTAINER_NAME": "app", "K8S_DEPLOYMENT_NAME": "web", "K8S_CLUSTER_NAME": "c1"}, ns)
	res, err := KubernetesDetector{}.Detect(context.Background())
	if err != nil || value(res, "k8s.pod.name") != "pod-abc" || value(res, "k8s.namespace.name") != "payments" ||
		value(res, "k8s.node.name") != "node-1" || value(res, "k8s.pod.uid") != "uid-1" || value(res, "k8s.container.name") != "app" ||
		value(res, "k8s.deployment.name") != "web" || value(res, "k8s.cluster.name") != "c1" {
		t.Fatalf("in cluster: %v %v", res, err)
	}

	withEnv(t, map[string]string{"HOSTNAME": "laptop", "K8S_POD_NAME": "explicit", "POD_NAMESPACE": "ns"}, ns)
	res, _ = KubernetesDetector{}.Detect(context.Background())
	if value(res, "k8s.pod.name") != "explicit" || value(res, "k8s.namespace.name") != "ns" {
		t.Fatalf("explicit vars: %v", res)
	}

	withEnv(t, map[string]string{"HOSTNAME": "laptop"}, ns)
	if res, _ := (KubernetesDetector{}).Detect(context.Background()); res.Len() != 0 {
		t.Fatalf("outside: %v", res)
	}
	withEnv(t, map[string]string{"KUBERNETES_SERVICE_HOST": "x"}, filepath.Join(dir, "missing"))
	if res, _ := (KubernetesDetector{}).Detect(context.Background()); res.Len() != 0 {
		t.Fatalf("nothing known: %v", res)
	}
}

func TestDetectResource(t *testing.T) {
	withEnv(t, map[string]string{"K8S_POD_NAME": "p"}, "/nonexistent")
	res, err := DetectResource(context.Background(), Config{ServiceName: "svc", ServiceVersion: "1.0", Environment: "prod",
		ResourceAttributes: map[string]any{"team": "core", "service.name": "overridden"}})
	if err != nil {
		t.Fatal(err)
	}
	if value(res, "service.name") != "svc" || value(res, "service.version") != "1.0" || value(res, "deployment.environment.name") != "prod" ||
		value(res, "deployment.environment") != "prod" || value(res, "team") != "core" || value(res, "host.name") == "" ||
		value(res, "process.pid") == "" || value(res, "k8s.pod.name") != "p" || value(res, "telemetry.sdk.language") != "go" {
		t.Fatalf("resource: %v", res)
	}
	res, err = DetectResource(context.Background(), Config{ServiceName: "svc", ResourceDetectors: []string{}})
	if err != nil || value(res, "host.name") != "" || value(res, "process.pid") != "" || value(res, "os.type") == "" {
		t.Fatalf("no detectors: %v %v", res, err)
	}
	res, _ = DetectResource(context.Background(), Config{ResourceDetectors: []string{DetectorContainer}})
	if value(res, "service.name") != "" && value(res, "host.name") != "" {
		t.Fatalf("container only: %v", res)
	}
}
