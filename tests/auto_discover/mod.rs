#![cfg(feature = "docker-tests")]

mod forwarding;
mod local_services;
mod port_binding;
mod preserve_src_ip;
mod recovery;
mod registration;
mod startup_race;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Once;

static INIT: Once = Once::new();

/// A test that hits `exit 1` never reaches the `teardown()` fragment appended to
/// its script, so its `it-*` container survives on the host and the next run dies
/// at `docker run --name` with a conflict that masks the real failure. Anchored
/// `^it-` so a loose match cannot reach names like `audit-it-decoy`. Best effort:
/// a missing or unhappy `docker` must never turn the suite red.
fn sweep_leaked_containers() {
    let Ok(list) = Command::new("docker")
        .args(["ps", "-aq", "--filter", "name=^it-"])
        .output()
    else {
        return;
    };
    let ids: Vec<String> = String::from_utf8_lossy(&list.stdout)
        .lines()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(String::from)
        .collect();
    if ids.is_empty() {
        return;
    }
    eprintln!(
        "sweeping {} leaked it-* test container(s) from a previous run: {}",
        ids.len(),
        ids.join(" ")
    );
    let _ = Command::new("docker")
        .args(["rm", "-f"])
        .args(&ids)
        .status();
}

fn setup_image() -> &'static str {
    let image_name = "lab-ops-auto-discover-test:latest";
    INIT.call_once(|| {
        sweep_leaked_containers();
        let dockerfile = concat!(
            "FROM ubuntu:24.04\n",
            "RUN apt-get update && apt-get install -y iptables jq curl unzip iproute2 docker.io\n",
            "RUN curl -fsSL https://releases.hashicorp.com/consul/1.19.2/consul_1.19.2_linux_amd64.zip ",
            "-o /tmp/consul.zip && unzip /tmp/consul.zip -d /usr/local/bin && rm /tmp/consul.zip\n",
        );
        let mut child = Command::new("docker")
            .args(["build", "-t", image_name, "-"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .expect("Failed to spawn docker build for auto-discover test image");
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().expect("Failed to open stdin");
            stdin
                .write_all(dockerfile.as_bytes())
                .expect("Failed to write Dockerfile");
        }
        let status = child.wait().expect("Failed to wait for docker build");
        assert!(status.success(), "Failed to build auto-discover test image");
    });
    image_name
}

/// A NixOS host links lab-ops against a loader and an OpenSSL under
/// /nix/store that the test image lacks, so the binary cannot exec.
/// Mounting /nix fixes the loader, but the lib still needs to be on the
/// loader path, and setting LD_LIBRARY_PATH for the whole container
/// shadows the image's own OpenSSL-linked tools: nix libcrypto's RUNPATH
/// pulls nix glibc's libdl into curl, which then fails against the
/// image's glibc. So put the path on a wrapper around the binary alone.
/// Returns the wrapper to bind-mount over lab-ops, or None off NixOS.
fn nix_wrapper(label: &str) -> Option<PathBuf> {
    if !Path::new("/nix").is_dir() {
        return None;
    }
    let lib_dir = std::env::var("OPENSSL_LIB_DIR").ok()?;
    let wrapper = std::env::temp_dir().join(format!("lab-ops-{label}-wrapper.sh"));
    let script = format!(
        "#!/bin/sh\nexec env LD_LIBRARY_PATH={lib_dir} /usr/local/bin/lab-ops.bin \"$@\"\n"
    );
    std::fs::write(&wrapper, script).ok()?;
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).ok()?;
    Some(wrapper)
}

pub(crate) fn run(script: &str) -> String {
    let image = setup_image();
    let binary_path = env!("CARGO_BIN_EXE_lab-ops");
    let mut cmd = Command::new("docker");
    cmd.args([
        "run",
        "--rm",
        "--privileged",
        "-v",
        "/var/run/docker.sock:/var/run/docker.sock",
        "-e",
        "NATMAP_SOCKET=/tmp/natmap.sock",
        "-e",
        "CONSUL_HTTP_ADDR=http://127.0.0.1:8500",
    ]);
    match nix_wrapper("auto-discover-docker") {
        Some(wrapper) => {
            cmd.args([
                "-v",
                "/nix:/nix:ro",
                "-v",
                &format!("{binary_path}:/usr/local/bin/lab-ops.bin"),
                "-v",
                &format!("{}:/usr/local/bin/lab-ops:ro", wrapper.display()),
            ]);
        }
        None => {
            cmd.args(["-v", &format!("{binary_path}:/usr/local/bin/lab-ops")]);
        }
    }
    cmd.args([image, "sh", "-c"]);
    cmd.arg(script);

    let output = cmd.output().expect("Failed to execute docker run");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!("Test container failed.\nstdout:\n{stdout}\nstderr:\n{stderr}");
    }
    stdout
}

/// Cleanup helper. Kills all background jobs by PID and removes Docker containers.
pub(crate) fn teardown(container_names: &[&str]) -> String {
    let removes: String = container_names
        .iter()
        .map(|n| format!("docker rm -f {n} 2>/dev/null || true"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"
for pid in $(jobs -p 2>/dev/null); do
  kill $pid 2>/dev/null || true
done
for i in $(seq 1 10); do
  remaining=$(jobs -p 2>/dev/null | wc -l)
  [ "$remaining" -eq 0 ] && break
  sleep 0.2
done
{removes}
"#
    )
}

pub(crate) fn assert_pass(output: &str, test_name: &str) {
    assert!(
        output.contains("PASS"),
        "{test_name} failed.\nOutput:\n{output}"
    );
}

/// Writes new-format YAML config via extra_setup overwrite.
/// The services_yaml must contain the `services:` block.
pub(crate) fn new_format_setup(services_yaml: &str, extra_setup: &str) -> String {
    new_format_setup_with_defaults_ext(services_yaml, "", extra_setup, "--no-forwarding")
}

pub(crate) fn new_format_setup_with_defaults_ext(
    services_yaml: &str,
    defaults_yaml: &str,
    extra_setup: &str,
    daemon_flags: &str,
) -> String {
    let defaults_block = if defaults_yaml.is_empty() {
        String::new()
    } else {
        format!("defaults:\n{defaults_yaml}\n")
    };
    let full_yaml = format!("node:\n  name: int-test-node\n\n{defaults_block}{services_yaml}");
    format!(
        r#"
set -e
export NATMAP_SOCKET=/tmp/natmap.sock
export CONSUL_HTTP_ADDR=http://127.0.0.1:8500

consul agent -dev -http-port=8500 -pid-file=/tmp/consul.pid >/tmp/consul.log 2>&1 &
for i in $(seq 1 20); do kill -0 $! 2>/dev/null && break; sleep 0.2; done
if ! kill -0 $! 2>/dev/null; then echo "FAIL: consul died" >&2; cat /tmp/consul.log; exit 1; fi

ip link add dummy0 type dummy 2>/dev/null || true
ip addr add 10.99.99.1/24 dev dummy0 2>/dev/null || true
ip link set dummy0 up

rm -f /tmp/natmap_state.json
lab-ops natmap daemon --socket /tmp/natmap.sock --state /tmp/natmap_state.json --socket-group root >/tmp/natmap.log 2>&1 &
for i in $(seq 1 20); do [ -S /tmp/natmap.sock ] && break; sleep 0.2; done
if ! kill -0 $! 2>/dev/null; then echo "FAIL: natmap daemon died" >&2; cat /tmp/natmap.log; exit 1; fi

cat > /tmp/discovery.yaml <<'YAMLEOF'
{full_yaml}
YAMLEOF
{extra_setup}

lab-ops auto-discover daemon /tmp/discovery.yaml \
    {daemon_flags} \
    --consul-addr http://127.0.0.1:8500 \
    >/tmp/discovery.log 2>&1 &
for i in $(seq 1 20); do kill -0 $! 2>/dev/null && break; sleep 0.2; done
if ! kill -0 $! 2>/dev/null; then echo "FAIL: auto-discover daemon died" >&2; cat /tmp/discovery.log; exit 1; fi
"#
    )
}

// --- Consul wait helpers ---

/// Emits a shell snippet that polls Consul agent services until `service_name`
/// appears, up to `max_wait_secs` seconds. Exits the script with FAIL if the
/// service never registers.
pub(crate) fn wait_for_consul_service(service_name: &str, max_wait_secs: u32) -> String {
    format!(
        r#"
for i in $(seq 1 {max_wait_secs}); do
  SVC=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services 2>/dev/null | jq -r 'to_entries[] | select(.value.Service == "{service_name}") | .key // empty' 2>/dev/null)
  if [ -n "$SVC" ]; then break; fi
  sleep 1
done
if [ -z "$SVC" ]; then
  echo "FAIL: service {service_name} never registered with Consul" >&2
  cat /tmp/discovery.log
  exit 1
fi
"#
    )
}
