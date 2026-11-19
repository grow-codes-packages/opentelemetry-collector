// Service tests (OpenTelemetry Collector): resource detection.
//! Host, process, container and Kubernetes detectors with injected inputs.

use std::collections::HashMap;

use grow_opentelemetry_collector as sdk;
use sdk::resource::{
    container_id, detect_resource_attributes_with, DetectOptions, K8S_NAMESPACE_FILE,
};
use sdk::{ErrorCode, ResourceDetector};
use serde_json::json;

const ID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn detectors_parse() {
    assert_eq!(
        ResourceDetector::parse_list(" host ,kubernetes,,host").unwrap(),
        vec![ResourceDetector::Host, ResourceDetector::K8s]
    );
    assert!(ResourceDetector::parse_list("host,none")
        .unwrap()
        .is_empty());
    assert_eq!(
        ResourceDetector::parse_list("cloud").unwrap_err().code(),
        ErrorCode::Config
    );
    let names: Vec<&str> = ResourceDetector::ALL.iter().map(|d| d.as_str()).collect();
    assert_eq!(names, ["host", "process", "container", "k8s"]);
}

#[test]
fn host_and_process() {
    let env: HashMap<String, String> = [("HOSTNAME".to_owned(), " box-1 ".to_owned())].into();
    let attrs = detect_resource_attributes_with(
        &[ResourceDetector::Host, ResourceDetector::Process],
        &DetectOptions {
            env: Some(&env),
            read_file: Some(&|_| None),
        },
    );
    assert_eq!(attrs["host.name"], json!("box-1"));
    assert!(attrs.contains_key("host.arch"));
    assert_eq!(attrs["os.type"], json!(std::env::consts::OS));
    assert_eq!(attrs["process.pid"], json!(std::process::id()));
    assert_eq!(attrs["process.runtime.name"], json!("rust"));
    assert!(attrs.contains_key("process.executable.name"));
    assert!(attrs.contains_key("process.command_args"));
    // Without HOSTNAME, /etc/hostname is read.
    let empty = HashMap::new();
    let attrs = detect_resource_attributes_with(
        &[ResourceDetector::Host],
        &DetectOptions {
            env: Some(&empty),
            read_file: Some(&|p| (p == "/etc/hostname").then(|| "from-file\n".to_owned())),
        },
    );
    assert_eq!(attrs["host.name"], json!("from-file"));
    // Real environment and file system.
    let attrs = sdk::detect_resource_attributes(&ResourceDetector::ALL);
    assert!(attrs.contains_key("process.pid"));
    assert!(sdk::detect_resource_attributes(&[]).is_empty());
}

#[test]
fn kubernetes() {
    let env: HashMap<String, String> = [
        ("KUBERNETES_SERVICE_HOST", "10.0.0.1"),
        ("HOSTNAME", "pod-abc"),
        ("POD_UID", "uid-1"),
        ("NODE_NAME", "node-1"),
        ("K8S_CONTAINER_NAME", "app"),
        ("K8S_DEPLOYMENT_NAME", "web"),
        ("K8S_CLUSTER_NAME", "c1"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    let read = |p: &str| (p == K8S_NAMESPACE_FILE).then(|| "shop\n".to_owned());
    let attrs = detect_resource_attributes_with(
        &[ResourceDetector::K8s],
        &DetectOptions {
            env: Some(&env),
            read_file: Some(&read),
        },
    );
    assert_eq!(attrs["k8s.pod.name"], json!("pod-abc"));
    assert_eq!(attrs["k8s.namespace.name"], json!("shop"));
    assert_eq!(attrs["k8s.pod.uid"], json!("uid-1"));
    assert_eq!(attrs["k8s.node.name"], json!("node-1"));
    assert_eq!(attrs["k8s.container.name"], json!("app"));
    assert_eq!(attrs["k8s.deployment.name"], json!("web"));
    assert_eq!(attrs["k8s.cluster.name"], json!("c1"));
    // Explicit variables win; outside a cluster HOSTNAME and the file are ignored.
    let env: HashMap<String, String> = [
        ("K8S_POD_NAME", "explicit-pod"),
        ("POD_NAMESPACE", "ns"),
        ("HOSTNAME", "ignored"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    let attrs = detect_resource_attributes_with(
        &[ResourceDetector::K8s],
        &DetectOptions {
            env: Some(&env),
            read_file: Some(&read),
        },
    );
    assert_eq!(attrs["k8s.pod.name"], json!("explicit-pod"));
    assert_eq!(attrs["k8s.namespace.name"], json!("ns"));
    let none = HashMap::new();
    let attrs = detect_resource_attributes_with(
        &[ResourceDetector::K8s],
        &DetectOptions {
            env: Some(&none),
            read_file: Some(&read),
        },
    );
    assert!(attrs.is_empty());
    assert!(format!(
        "{:?}",
        DetectOptions {
            env: None,
            read_file: None
        }
    )
    .contains("DetectOptions"));
}

#[test]
fn containers() {
    let v1 = format!("12:pids:/docker/{ID}\n0::/\n");
    assert_eq!(
        container_id(&|p| (p == "/proc/self/cgroup").then(|| v1.clone())).as_deref(),
        Some(ID)
    );
    let systemd = format!("0::/system.slice/docker-{ID}.scope\n");
    assert_eq!(
        container_id(&|p| (p == "/proc/self/cgroup").then(|| systemd.clone())).as_deref(),
        Some(ID)
    );
    let mountinfo = format!(
        "1 2 0:1 / / rw - overlay\n640 638 254:1 /var/lib/docker/containers/{ID}/hostname /etc/hostname rw - ext4\n"
    );
    let read = |p: &str| match p {
        "/proc/self/cgroup" => Some("0::/\n".to_owned()),
        "/proc/self/mountinfo" => Some(mountinfo.clone()),
        _ => None,
    };
    assert_eq!(container_id(&read).as_deref(), Some(ID));
    let sandbox = format!("5 6 0:1 /k8s/sandboxes/{ID}/hostname /etc/hostname rw\n");
    assert_eq!(
        container_id(&|p| (p == "/proc/self/mountinfo").then(|| sandbox.clone())).as_deref(),
        Some(ID)
    );
    assert_eq!(container_id(&|_| Some("0::/\n".to_owned())), None);
    assert_eq!(container_id(&|_| None), None);
    let attrs = detect_resource_attributes_with(
        &[ResourceDetector::Container],
        &DetectOptions {
            env: None,
            read_file: Some(&read),
        },
    );
    assert_eq!(attrs["container.id"], json!(ID));
}

#[test]
fn telemetry_resource() {
    let cfg = sdk::OtelCollectorConfig::builder()
        .service_name("svc")
        .resource_attribute("host.name", "override")
        .resource_detectors(vec![ResourceDetector::Host])
        .build()
        .unwrap();
    let attrs = sdk::telemetry_resource_attributes(&cfg);
    assert_eq!(attrs["host.name"], json!("override"));
    assert_eq!(attrs["service.name"], json!("svc"));
    let res = sdk::detect_resource(&cfg);
    assert!(res
        .iter()
        .any(|(k, v)| k.as_str() == "service.name" && v.as_str() == "svc"));
}
