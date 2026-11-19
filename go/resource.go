package otelcollector

import (
	"context"
	"errors"
	"os"
	"strings"

	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/sdk/resource"
)

// k8sNamespaceFile is the service account namespace file mounted in every pod.
var k8sNamespaceFile = "/var/run/secrets/kubernetes.io/serviceaccount/namespace"

// lookupEnv is the environment lookup of the Kubernetes detector (replaced in tests).
var lookupEnv = os.LookupEnv

// KubernetesDetector is a best-effort resource.Detector for pods using downward-API
// variables: k8s.pod.name (K8S_POD_NAME, POD_NAME; HOSTNAME inside a cluster),
// k8s.namespace.name (K8S_NAMESPACE_NAME, POD_NAMESPACE; the service account namespace
// file inside a cluster), k8s.pod.uid (K8S_POD_UID, POD_UID), k8s.node.name
// (K8S_NODE_NAME, NODE_NAME), k8s.container.name (K8S_CONTAINER_NAME, CONTAINER_NAME),
// k8s.deployment.name (K8S_DEPLOYMENT_NAME) and k8s.cluster.name (K8S_CLUSTER_NAME).
// "Inside a cluster" means KUBERNETES_SERVICE_HOST is set. It reports nothing when no
// variable is set.
type KubernetesDetector struct{}

var _ resource.Detector = KubernetesDetector{}

func firstEnv(names ...string) string {
	for _, n := range names {
		if v, ok := lookupEnv(n); ok && strings.TrimSpace(v) != "" {
			return strings.TrimSpace(v)
		}
	}
	return ""
}

// Detect implements resource.Detector.
func (KubernetesDetector) Detect(context.Context) (*resource.Resource, error) {
	inCluster := firstEnv("KUBERNETES_SERVICE_HOST") != ""
	var attrs []attribute.KeyValue
	add := func(key, value string) {
		if value != "" {
			attrs = append(attrs, attribute.String(key, value))
		}
	}
	pod := firstEnv("K8S_POD_NAME", "POD_NAME")
	if pod == "" && inCluster {
		pod = firstEnv("HOSTNAME")
	}
	add("k8s.pod.name", pod)
	ns := firstEnv("K8S_NAMESPACE_NAME", "POD_NAMESPACE")
	if ns == "" && inCluster {
		if data, err := os.ReadFile(k8sNamespaceFile); err == nil {
			ns = strings.TrimSpace(string(data))
		}
	}
	add("k8s.namespace.name", ns)
	add("k8s.pod.uid", firstEnv("K8S_POD_UID", "POD_UID"))
	add("k8s.node.name", firstEnv("K8S_NODE_NAME", "NODE_NAME"))
	add("k8s.container.name", firstEnv("K8S_CONTAINER_NAME", "CONTAINER_NAME"))
	add("k8s.deployment.name", firstEnv("K8S_DEPLOYMENT_NAME"))
	add("k8s.cluster.name", firstEnv("K8S_CLUSTER_NAME"))
	if len(attrs) == 0 {
		return resource.Empty(), nil
	}
	return resource.NewSchemaless(attrs...), nil
}

// DetectResource builds the shared telemetry resource of cfg: the SDK and OS
// attributes, the detectors in cfg.ResourceDetectors (nil = all: host, process,
// container, k8s; best effort, partial results are kept), then ResourceAttributes,
// then service.name, service.version and deployment.environment.name (+ the older
// deployment.environment).
func DetectResource(ctx context.Context, cfg Config) (*resource.Resource, error) {
	detectors := cfg.ResourceDetectors
	if detectors == nil {
		detectors = AllResourceDetectors
	}
	opts := []resource.Option{resource.WithTelemetrySDK(), resource.WithOS()}
	for _, d := range detectors {
		switch d {
		case DetectorHost:
			opts = append(opts, resource.WithHost())
		case DetectorProcess:
			opts = append(opts, resource.WithProcessPID(), resource.WithProcessExecutableName(),
				resource.WithProcessRuntimeName(), resource.WithProcessRuntimeVersion())
		case DetectorContainer:
			opts = append(opts, resource.WithContainer())
		case DetectorK8s:
			opts = append(opts, resource.WithDetectors(KubernetesDetector{}))
		}
	}
	attrs := OTelAttributes(cfg.ResourceAttributes)
	if cfg.ServiceName != "" {
		attrs = append(attrs, attribute.String("service.name", cfg.ServiceName))
	}
	if cfg.ServiceVersion != "" {
		attrs = append(attrs, attribute.String("service.version", cfg.ServiceVersion))
	}
	if cfg.Environment != "" {
		attrs = append(attrs, attribute.String("deployment.environment.name", cfg.Environment),
			attribute.String("deployment.environment", cfg.Environment))
	}
	opts = append(opts, resource.WithAttributes(attrs...))
	res, err := resource.New(ctx, opts...)
	if err != nil && !errors.Is(err, resource.ErrPartialResource) {
		return nil, ConfigError("resource detection failed: " + err.Error())
	}
	return res, nil
}
