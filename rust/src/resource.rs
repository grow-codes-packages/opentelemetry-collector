// Service code (OpenTelemetry Collector): best-effort resource detection.
//! Resource detection for `init_telemetry` and `init_tracing`: host, process, container and
//! Kubernetes attributes, all best effort (a detector that finds nothing adds nothing).

use std::collections::HashMap;
use std::str::FromStr;

use opentelemetry_sdk::Resource;

use crate::config::OtelCollectorConfig;
use crate::core::{Error, Result};
use crate::logging::{AttrValue, Attributes};
use crate::tracing::otel_attributes;

/// Service-account namespace file mounted in every Kubernetes pod.
pub const K8S_NAMESPACE_FILE: &str = "/var/run/secrets/kubernetes.io/serviceaccount/namespace";

/// A resource detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceDetector {
    /// `host.name`, `host.arch`, `os.type`.
    Host,
    /// `process.pid`, `process.executable.*`, `process.command*`, `process.runtime.*`.
    Process,
    /// `container.id` from `/proc/self/cgroup` (v1) or `/proc/self/mountinfo` (v2).
    Container,
    /// `k8s.*` from the downward-API variables and the service-account namespace file.
    K8s,
}

impl ResourceDetector {
    /// Every detector (the default).
    pub const ALL: [ResourceDetector; 4] = [
        ResourceDetector::Host,
        ResourceDetector::Process,
        ResourceDetector::Container,
        ResourceDetector::K8s,
    ];

    /// Wire name: `host`, `process`, `container`, `k8s`.
    pub fn as_str(self) -> &'static str {
        match self {
            ResourceDetector::Host => "host",
            ResourceDetector::Process => "process",
            ResourceDetector::Container => "container",
            ResourceDetector::K8s => "k8s",
        }
    }

    /// Parses a comma-separated list; `none` means no detector.
    pub fn parse_list(raw: &str) -> Result<Vec<ResourceDetector>> {
        let mut out = Vec::new();
        for part in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            if part.eq_ignore_ascii_case("none") {
                return Ok(Vec::new());
            }
            let d: ResourceDetector = part.parse()?;
            if !out.contains(&d) {
                out.push(d);
            }
        }
        Ok(out)
    }
}

impl FromStr for ResourceDetector {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "host" => Ok(ResourceDetector::Host),
            "process" => Ok(ResourceDetector::Process),
            "container" => Ok(ResourceDetector::Container),
            "k8s" | "kubernetes" => Ok(ResourceDetector::K8s),
            other => Err(Error::config(format!(
                "unknown resource detector {other:?} (use host, process, container or k8s)"
            ))),
        }
    }
}

/// Reads a file; `None` when it is missing.
pub type FileReader<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Inputs of [`detect_resource_attributes_with`] (tests replace the environment and the file
/// reader).
pub struct DetectOptions<'a> {
    /// Environment variables (`None` = the process environment).
    pub env: Option<&'a HashMap<String, String>>,
    /// Reads a file; `None` when missing (`None` = the real file system).
    pub read_file: Option<FileReader<'a>>,
}

impl std::fmt::Debug for DetectOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DetectOptions").finish_non_exhaustive()
    }
}

/// Detects resource attributes with the given detectors from the process environment and the
/// real file system (see [`detect_resource_attributes_with`]).
pub fn detect_resource_attributes(detectors: &[ResourceDetector]) -> Attributes {
    detect_resource_attributes_with(
        detectors,
        &DetectOptions {
            env: None,
            read_file: None,
        },
    )
}

/// Detects resource attributes: `host` (`host.name`, `host.arch`, `os.type`), `process`
/// (`process.pid`, `process.executable.name`, `process.executable.path`, `process.command`,
/// `process.command_args`, `process.runtime.name`), `container` (`container.id` from cgroups)
/// and `k8s` (`k8s.pod.name`, `k8s.namespace.name`, `k8s.pod.uid`, `k8s.node.name`,
/// `k8s.container.name`, `k8s.deployment.name`, `k8s.cluster.name` from `K8S_*` / `POD_*` /
/// `NODE_NAME` / `CONTAINER_NAME`; inside a cluster (`KUBERNETES_SERVICE_HOST` set) also
/// `HOSTNAME` and the service-account namespace file).
pub fn detect_resource_attributes_with(
    detectors: &[ResourceDetector],
    opts: &DetectOptions<'_>,
) -> Attributes {
    let env = |name: &str| -> Option<String> {
        let v = match opts.env {
            Some(map) => map.get(name).cloned(),
            None => std::env::var(name).ok(),
        }?;
        let v = v.trim().to_owned();
        (!v.is_empty()).then_some(v)
    };
    let read = |path: &str| -> Option<String> {
        match opts.read_file {
            Some(f) => f(path),
            None => std::fs::read_to_string(path).ok(),
        }
    };
    let mut out = Attributes::new();
    let mut set = |k: &str, v: Option<String>| {
        if let Some(v) = v {
            out.insert(k.to_owned(), AttrValue::String(v));
        }
    };
    for d in detectors {
        match d {
            ResourceDetector::Host => {
                set(
                    "host.name",
                    env("HOSTNAME").or_else(|| read("/etc/hostname").map(|s| s.trim().to_owned())),
                );
                set("host.arch", Some(host_arch(std::env::consts::ARCH)));
                set("os.type", Some(std::env::consts::OS.to_owned()));
            }
            ResourceDetector::Process => {
                let exe = std::env::current_exe().ok();
                set(
                    "process.executable.name",
                    exe.as_ref()
                        .and_then(|p| p.file_name())
                        .map(|n| n.to_string_lossy().into_owned()),
                );
                set(
                    "process.executable.path",
                    exe.as_ref().map(|p| p.to_string_lossy().into_owned()),
                );
                let mut args = std::env::args();
                set("process.command", args.next());
                set("process.runtime.name", Some("rust".to_owned()));
            }
            ResourceDetector::Container => {
                set("container.id", container_id(&read));
            }
            ResourceDetector::K8s => {
                let pick = |names: &[&str]| names.iter().find_map(|n| env(n));
                let in_cluster = env("KUBERNETES_SERVICE_HOST").is_some();
                set(
                    "k8s.pod.name",
                    pick(&["K8S_POD_NAME", "POD_NAME"])
                        .or_else(|| in_cluster.then(|| env("HOSTNAME")).flatten()),
                );
                set(
                    "k8s.namespace.name",
                    pick(&["K8S_NAMESPACE_NAME", "POD_NAMESPACE"]).or_else(|| {
                        in_cluster
                            .then(|| read(K8S_NAMESPACE_FILE))
                            .flatten()
                            .map(|s| s.trim().to_owned())
                            .filter(|s| !s.is_empty())
                    }),
                );
                set("k8s.pod.uid", pick(&["K8S_POD_UID", "POD_UID"]));
                set("k8s.node.name", pick(&["K8S_NODE_NAME", "NODE_NAME"]));
                set(
                    "k8s.container.name",
                    pick(&["K8S_CONTAINER_NAME", "CONTAINER_NAME"]),
                );
                set("k8s.deployment.name", pick(&["K8S_DEPLOYMENT_NAME"]));
                set("k8s.cluster.name", pick(&["K8S_CLUSTER_NAME"]));
            }
        }
    }
    if detectors.contains(&ResourceDetector::Process) {
        out.insert("process.pid".into(), AttrValue::from(std::process::id()));
        let args: Vec<AttrValue> = std::env::args().map(AttrValue::String).collect();
        if !args.is_empty() {
            out.insert("process.command_args".into(), AttrValue::Array(args));
        }
    }
    out
}

fn host_arch(arch: &str) -> String {
    match arch {
        "x86_64" => "amd64".to_owned(),
        "aarch64" => "arm64".to_owned(),
        "x86" => "x86".to_owned(),
        other => other.to_owned(),
    }
}

/// Container id from `/proc/self/cgroup` (v1) or `/proc/self/mountinfo` (v2); `None` if none.
pub fn container_id(read_file: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let is_id = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
    if let Some(cgroup) = read_file("/proc/self/cgroup") {
        for line in cgroup.lines() {
            let last = line.rsplit('/').next().unwrap_or("");
            let last = last.trim_end_matches(".scope");
            let candidate = last.rsplit(['-', ':']).next().unwrap_or("");
            if is_id(candidate) {
                return Some(candidate.to_owned());
            }
        }
    }
    if let Some(mountinfo) = read_file("/proc/self/mountinfo") {
        for line in mountinfo.lines() {
            if !line.contains("hostname") {
                continue;
            }
            for marker in ["/containers/", "/sandboxes/"] {
                if let Some(pos) = line.find(marker) {
                    let rest = &line[pos + marker.len()..];
                    let id = rest.split('/').next().unwrap_or("");
                    if is_id(id) {
                        return Some(id.to_owned());
                    }
                }
            }
        }
    }
    None
}

/// The shared telemetry resource attributes of a config: detected (`config.resource_detectors`)
/// < `resource_attributes` < `service.name`, `service.version`, `deployment.environment.name`
/// and `deployment.environment`.
pub fn telemetry_resource_attributes(config: &OtelCollectorConfig) -> Attributes {
    let mut out = detect_resource_attributes(config.resource_detectors());
    out.extend(config.resource_map());
    out
}

/// The OpenTelemetry resource of a config: the SDK defaults (`telemetry.sdk.*`,
/// `OTEL_RESOURCE_ATTRIBUTES`) merged with [`telemetry_resource_attributes`].
pub fn detect_resource(config: &OtelCollectorConfig) -> Resource {
    resource_from_attributes(&telemetry_resource_attributes(config))
}

pub(crate) fn resource_from_attributes(attrs: &Attributes) -> Resource {
    let service = attrs
        .get("service.name")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let mut b = Resource::builder().with_attributes(otel_attributes(attrs));
    if let Some(s) = service {
        b = b.with_service_name(s);
    }
    b.build()
}
