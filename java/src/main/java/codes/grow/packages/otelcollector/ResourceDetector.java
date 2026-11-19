package codes.grow.packages.otelcollector;

import java.io.IOException;
import java.net.InetAddress;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.Collection;
import java.util.LinkedHashMap;
import java.util.Locale;
import java.util.Map;
import java.util.Optional;
import java.util.function.Function;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/**
 * Best-effort client-side resource detection: {@code host} ({@code host.name}, {@code host.arch},
 * {@code os.type}, {@code os.version}), {@code process} ({@code process.pid}, {@code
 * process.executable.path}, {@code process.runtime.*}, {@code process.owner}), {@code container}
 * ({@code container.id} from cgroups) and {@code k8s} ({@code k8s.pod.name}, {@code
 * k8s.namespace.name}, … from the downward-API variables {@code K8S_*} / {@code POD_*}, {@code
 * HOSTNAME} and the service-account namespace file). Detectors that find nothing add nothing.
 */
public final class ResourceDetector {
  private static final Pattern CONTAINER_ID = Pattern.compile("([0-9a-f]{64})");
  private static final Pattern MOUNT_ID =
      Pattern.compile("/(?:containers|sandboxes)/([0-9a-f]{64})/");
  private static final String K8S_NAMESPACE_FILE =
      "/var/run/secrets/kubernetes.io/serviceaccount/namespace";

  private final Map<String, String> env;
  private final Function<String, Optional<String>> readFile;

  private ResourceDetector(Map<String, String> env, Function<String, Optional<String>> readFile) {
    this.env = env;
    this.readFile = readFile;
  }

  /**
   * A detector over the process environment and file system.
   *
   * @return the detector
   */
  public static ResourceDetector system() {
    return new ResourceDetector(System.getenv(), ResourceDetector::read);
  }

  /**
   * A detector over explicit sources (tests).
   *
   * @param env environment variables
   * @param readFile reads a text file, empty when it cannot
   * @return the detector
   */
  public static ResourceDetector of(
      Map<String, String> env, Function<String, Optional<String>> readFile) {
    return new ResourceDetector(Map.copyOf(env), readFile);
  }

  private static Optional<String> read(String path) {
    try {
      return Optional.of(Files.readString(Paths.get(path), StandardCharsets.UTF_8));
    } catch (IOException | RuntimeException e) {
      return Optional.empty();
    }
  }

  /**
   * Runs the given detectors.
   *
   * @param detectors names: {@code host}, {@code process}, {@code container}, {@code k8s}
   * @return the detected attributes
   */
  public Map<String, Object> detect(Collection<String> detectors) {
    Map<String, Object> out = new LinkedHashMap<>();
    if (detectors.contains("host")) {
      out.putAll(host());
    }
    if (detectors.contains("process")) {
      out.putAll(process());
    }
    if (detectors.contains("container")) {
      containerId().ifPresent(id -> out.put("container.id", id));
    }
    if (detectors.contains("k8s")) {
      out.putAll(k8s());
    }
    return out;
  }

  private Map<String, Object> host() {
    Map<String, Object> out = new LinkedHashMap<>();
    String name = env.get("HOSTNAME");
    if (name == null || name.isBlank()) {
      try {
        name = InetAddress.getLocalHost().getHostName();
      } catch (IOException e) {
        name = null;
      }
    }
    if (name != null && !name.isBlank()) {
      out.put("host.name", name.trim());
    }
    out.put("host.arch", arch(System.getProperty("os.arch", "")));
    String os = System.getProperty("os.name", "").toLowerCase(Locale.ROOT);
    out.put("os.type", os.startsWith("windows") ? "windows" : os.replace(" ", ""));
    out.put("os.version", System.getProperty("os.version", ""));
    return out;
  }

  static String arch(String raw) {
    switch (raw) {
      case "amd64":
      case "x86_64":
        return "amd64";
      case "aarch64":
      case "arm64":
        return "arm64";
      case "x86":
      case "i386":
        return "x86";
      default:
        return raw;
    }
  }

  private Map<String, Object> process() {
    Map<String, Object> out = new LinkedHashMap<>();
    out.put("process.pid", ProcessHandle.current().pid());
    Path java = Paths.get(System.getProperty("java.home", ""), "bin", "java");
    out.put("process.executable.path", java.toString());
    out.put("process.runtime.name", System.getProperty("java.runtime.name", "Java"));
    out.put("process.runtime.version", System.getProperty("java.runtime.version", ""));
    out.put(
        "process.runtime.description",
        System.getProperty("java.vm.vendor", "")
            + " "
            + System.getProperty("java.vm.name", "")
            + " "
            + System.getProperty("java.vm.version", ""));
    String user = System.getProperty("user.name");
    if (user != null && !user.isBlank()) {
      out.put("process.owner", user);
    }
    return out;
  }

  /**
   * Container id from {@code /proc/self/cgroup} (cgroup v1) or {@code /proc/self/mountinfo} (cgroup
   * v2).
   *
   * @return the 64-hex id, or empty
   */
  public Optional<String> containerId() {
    Optional<String> cgroup = readFile.apply("/proc/self/cgroup");
    if (cgroup.isPresent()) {
      for (String line : cgroup.get().split("\n")) {
        Matcher m = CONTAINER_ID.matcher(line.substring(line.lastIndexOf('/') + 1));
        if (m.find()) {
          return Optional.of(m.group(1));
        }
      }
    }
    Optional<String> mountinfo = readFile.apply("/proc/self/mountinfo");
    if (mountinfo.isPresent()) {
      for (String line : mountinfo.get().split("\n")) {
        if (!line.contains("hostname")) {
          continue;
        }
        Matcher m = MOUNT_ID.matcher(line);
        if (m.find()) {
          return Optional.of(m.group(1));
        }
      }
    }
    return Optional.empty();
  }

  private Optional<String> pick(String... names) {
    for (String n : names) {
      String v = env.get(n);
      if (v != null && !v.isBlank()) {
        return Optional.of(v.trim());
      }
    }
    return Optional.empty();
  }

  private Map<String, Object> k8s() {
    Map<String, Object> out = new LinkedHashMap<>();
    boolean inCluster = pick("KUBERNETES_SERVICE_HOST").isPresent();
    Optional<String> pod = pick("K8S_POD_NAME", "POD_NAME");
    if (pod.isEmpty() && inCluster) {
      pod = pick("HOSTNAME");
    }
    pod.ifPresent(v -> out.put("k8s.pod.name", v));
    Optional<String> ns = pick("K8S_NAMESPACE_NAME", "POD_NAMESPACE");
    if (ns.isEmpty() && inCluster) {
      ns = readFile.apply(K8S_NAMESPACE_FILE).map(String::trim).filter(s -> !s.isEmpty());
    }
    ns.ifPresent(v -> out.put("k8s.namespace.name", v));
    pick("K8S_POD_UID", "POD_UID").ifPresent(v -> out.put("k8s.pod.uid", v));
    pick("K8S_NODE_NAME", "NODE_NAME").ifPresent(v -> out.put("k8s.node.name", v));
    pick("K8S_CONTAINER_NAME", "CONTAINER_NAME").ifPresent(v -> out.put("k8s.container.name", v));
    pick("K8S_DEPLOYMENT_NAME").ifPresent(v -> out.put("k8s.deployment.name", v));
    pick("K8S_CLUSTER_NAME").ifPresent(v -> out.put("k8s.cluster.name", v));
    return out;
  }
}
