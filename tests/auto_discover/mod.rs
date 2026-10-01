//! Shared harness and shell helpers for the auto-discover Docker integration suite.

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

/// Runs `script` in the shared test container and returns its stdout.
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

/// Asserts the script printed a `PASS:` line, dumping the output on failure.
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

/// Like [`new_format_setup`], but also writes a `defaults:` block and starts the
/// daemon with `daemon_flags`. `services_yaml` must contain the `services:` block.
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
        "set -e\n{infra}\n{config}\n{extra_setup}\n{daemon}",
        infra = infra_setup(true),
        config = write_discovery_config(&full_yaml),
        daemon = start_daemon(daemon_flags),
    )
}

// --- Consul wait helpers ---

/// Emits a poll that runs `expr` up to `max_wait_secs` times, one per 0.5s,
/// and keeps the first result that is neither empty nor `null` in `VAR`. Exits
/// the script with FAIL (dumping the daemon log) if the condition never holds.
/// `|| true` guards the substitution: these scripts run under `set -e`, and an
/// expression that exits non-zero while the condition is false (`grep -c` with
/// no match, `docker inspect` on a removed container) would otherwise abort the
/// script on the first miss instead of retrying.
pub(crate) fn poll_until(expr: &str, var: &str, max_wait_secs: u32, what: &str) -> String {
    format!(
        r#"
{var}=""
for i in $(seq 1 {}); do
  {var}=$({expr} || true)
  if [ -n "${var}" ] && [ "${var}" != "null" ]; then break; fi
  sleep 0.5
done
if [ -z "${var}" ] || [ "${var}" = "null" ]; then
  echo "FAIL: {what} (waited {max_wait_secs}s)" >&2
  [ -f /tmp/discovery.log ] && cat /tmp/discovery.log >&2
  exit 1
fi
"#,
        max_wait_secs * 2
    )
}

/// Emits a bounded poll that runs `expr` until it comes back empty, then fails
/// if it never does. The inverse of `poll_until`, for a condition that has to
/// *disappear* (a deregistered Consul service, a removed ip rule): there is no
/// positive value to wait for, so the wait is on the absence.
pub(crate) fn poll_until_empty(expr: &str, max_wait_secs: u32, what: &str) -> String {
    format!(
        r#"
GONE=""
for i in $(seq 1 {iterations}); do
  GONE=$({expr} || true)
  if [ -z "$GONE" ]; then break; fi
  sleep 0.5
done
if [ -n "$GONE" ]; then
  echo "FAIL: {what} still present after {max_wait_secs}s ($GONE)" >&2
  [ -f /tmp/discovery.log ] && cat /tmp/discovery.log >&2
  exit 1
fi
"#,
        iterations = max_wait_secs * 2,
    )
}

/// A negative assertion ("this must NOT be registered") has no condition to
/// poll for, so it needs a grace period for the daemon to have processed the
/// event. Kept at the four seconds the unconditional wait it replaces, because
/// shortening it would make the absence vacuously true.
pub(crate) fn settle(secs: u32) -> String {
    format!("for i in $(seq 1 {}); do sleep 0.5; done\n", secs * 2)
}

/// The Consul agent service endpoint every wait expression reads from.
const CONSUL_SERVICES: &str = "curl -sf $CONSUL_HTTP_ADDR/v1/agent/services 2>/dev/null | jq";

/// The jq filter selecting the agent-service entries registered under
/// `service_name`.
fn consul_entries(service_name: &str) -> String {
    format!("to_entries[] | select(.value.Service == \"{service_name}\")")
}

/// The Consul agent-service IDs registered under `service_name`, empty when
/// there are none. `field` picks a different key when the test needs a value
/// rather than the entry itself.
fn consul_ids(service_name: &str, field: &str) -> String {
    format!(
        "{CONSUL_SERVICES} -r '{entries} | {field}' 2>/dev/null",
        entries = consul_entries(service_name)
    )
}

/// How many Consul entries `service_name` is registered under.
fn consul_count(service_name: &str) -> String {
    format!(
        "{CONSUL_SERVICES} '[{entries}] | length' 2>/dev/null",
        entries = consul_entries(service_name)
    )
}

/// Polls the Consul agent service list for `service_name` and leaves its Consul
/// ID in `SVC`.
pub(crate) fn wait_for_consul_service(service_name: &str, max_wait_secs: u32) -> String {
    poll_until(
        &consul_ids(service_name, ".key // empty"),
        "SVC",
        max_wait_secs,
        &format!("service {service_name} never registered with Consul"),
    )
}

/// Polls until `service_name` is gone from Consul, up to `max_wait_secs`. This
/// is the condition the live daemon produces when it deregisters a stopped
/// container's service.
pub(crate) fn wait_for_service_gone(service_name: &str, max_wait_secs: u32) -> String {
    poll_until_empty(
        &consul_ids(service_name, ".key // empty"),
        max_wait_secs,
        &format!("service {service_name} still registered with Consul"),
    )
}

/// Polls until exactly `count` Consul entries are registered for `service_name`,
/// leaving the count in `COUNT`. One service can register several entries (a
/// `rproxylocal` and a `forwardremote` side by side), so waiting for the first
/// entry is not enough to read the rest.
pub(crate) fn wait_for_service_count(
    service_name: &str,
    count: usize,
    max_wait_secs: u32,
) -> String {
    poll_until(
        &format!(
            "{CONSUL_SERVICES} '[{entries}] | if length == {count} then length else empty end' 2>/dev/null",
            entries = consul_entries(service_name)
        ),
        "COUNT",
        max_wait_secs,
        &format!("service {service_name} never reached {count} Consul entries"),
    )
}

/// Settles, then asserts that no *more* than `max` services named
/// `service_name` are registered.
pub(crate) fn assert_count_at_most(service_name: &str, max: usize, what: &str) -> String {
    format!(
        r#"{settle}SEEN=$({count})
if [ "$SEEN" -gt {max} ]; then echo "FAIL: expected at most {max} {service_name} entries, got $SEEN ({what})" >&2; exit 1; fi
"#,
        settle = settle(4),
        count = consul_count(service_name),
    )
}

/// Settles, then asserts that `service_name` is *not* registered.
pub(crate) fn assert_absent(service_name: &str, what: &str) -> String {
    format!(
        r#"{settle}STALE=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services 2>/dev/null | jq -r 'to_entries[] | select(.value.Service == "{service_name}") | .key // empty' 2>/dev/null)
if [ -n "$STALE" ]; then echo "FAIL: {service_name} {what} ($STALE)" >&2; exit 1; fi
"#,
        settle = settle(4),
    )
}

/// Emits an assertion that no service registered by this node's `server_name`
/// is left in Consul.
pub(crate) fn assert_node_registrations_gone() -> String {
    r#"
REMAINING=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Meta.server_name == "int-test-node") | .key // empty')
if [ -n "$REMAINING" ]; then echo "FAIL: stale registrations remain: $REMAINING" >&2; exit 1; fi
"#
    .to_string()
}

/// Polls the policy-routing table until `ip rule` carries a lookup for `table`,
/// leaving the rule list in `IP_RULE`. natmap adds the rule during the same
/// discovery pass that registers the service, so this is the condition the
/// policy-route tests actually assert on.
pub(crate) fn wait_for_ip_rule(table: u32, max_wait_secs: u32) -> String {
    poll_until(
        &ip_rule_expr(table),
        "IP_RULE",
        max_wait_secs,
        &format!("ip rule for table {table} never appeared"),
    )
}

/// The inverse of [`wait_for_ip_rule`]: polls until the lookup for `table` is
/// gone from `ip rule`, failing if it survives `max_wait_secs`.
pub(crate) fn wait_for_ip_rule_gone(table: u32, max_wait_secs: u32) -> String {
    poll_until_empty(
        &ip_rule_expr(table),
        max_wait_secs,
        &format!("ip rule for table {table}"),
    )
}

fn ip_rule_expr(table: u32) -> String {
    format!("ip rule show | grep 'lookup {table}' || true")
}

/// Writes `yaml` to the discovery config path. Every config write goes through
/// this so the path and heredoc quoting live in one place.
pub(crate) fn write_discovery_config(yaml: &str) -> String {
    format!(
        r#"cat > /tmp/discovery.yaml <<'YAMLEOF'
{yaml}
YAMLEOF
"#
    )
}

/// Starts the auto-discover daemon in the background, exporting
/// `DISCOVERY_PID`, and waits until it is actually alive.
pub(crate) fn start_daemon(daemon_flags: &str) -> String {
    format!(
        r#"
lab-ops auto-discover daemon /tmp/discovery.yaml \
    {daemon_flags} \
    --consul-addr http://127.0.0.1:8500 \
    >/tmp/discovery.log 2>&1 &
DISCOVERY_PID=$!
for i in $(seq 1 40); do kill -0 $DISCOVERY_PID 2>/dev/null && break; sleep 0.2; done
if ! kill -0 $DISCOVERY_PID 2>/dev/null; then echo "FAIL: auto-discover daemon died" >&2; cat /tmp/discovery.log; exit 1; fi
"#
    )
}

/// Stops the discovery daemon and waits for it to actually exit, so a
/// following one-shot `sync` is not racing a live daemon.
pub(crate) fn stop_daemon() -> String {
    r#"
kill $DISCOVERY_PID 2>/dev/null || true
for i in $(seq 1 40); do kill -0 $DISCOVERY_PID 2>/dev/null || break; sleep 0.2; done
"#
    .to_string()
}

/// Stops the natmap daemon started by [`infra_setup`] and waits for it to exit,
/// so a following command is not racing a live daemon holding the socket.
pub(crate) fn stop_natmap() -> String {
    r#"
kill $NATMAP_PID 2>/dev/null || true
for i in $(seq 1 40); do kill -0 $NATMAP_PID 2>/dev/null || break; sleep 0.2; done
"#
    .to_string()
}

/// Runs one-shot `auto-discover sync` against the current config.
pub(crate) fn sync_once() -> String {
    r#"lab-ops auto-discover sync /tmp/discovery.yaml >/tmp/sync.log 2>&1 || true
"#
    .to_string()
}

/// `set -e` plus consul, natmap and the discovery config, but no auto-discover
/// daemon. For tests that need to start the daemon themselves, in their own
/// order, after asserting something about the pre-daemon state.
pub(crate) fn base_setup(services_yaml: &str, with_dummy: bool) -> String {
    let full_yaml = format!("node:\n  name: int-test-node\n{services_yaml}");
    format!(
        "set -e\n{infra}\n{config}",
        infra = infra_setup(with_dummy),
        config = write_discovery_config(&full_yaml),
    )
}

/// Starts consul and the natmap daemon, exporting `NATMAP_PID`. `with_dummy`
/// adds the `dummy0` interface carrying 10.99.99.1/24.
pub(crate) fn infra_setup(with_dummy: bool) -> String {
    let dummy = if with_dummy {
        r#"
ip link add dummy0 type dummy 2>/dev/null || true
ip addr add 10.99.99.1/24 dev dummy0 2>/dev/null || true
ip link set dummy0 up
"#
    } else {
        ""
    };
    format!(
        r#"
export NATMAP_SOCKET=/tmp/natmap.sock
export CONSUL_HTTP_ADDR=http://127.0.0.1:8500

consul agent -dev -http-port=8500 -pid-file=/tmp/consul.pid >/tmp/consul.log 2>&1 &
for i in $(seq 1 40); do kill -0 $! 2>/dev/null && break; sleep 0.2; done
if ! kill -0 $! 2>/dev/null; then echo "FAIL: consul died" >&2; cat /tmp/consul.log; exit 1; fi
{dummy}
rm -f /tmp/natmap_state.json
lab-ops natmap daemon --socket /tmp/natmap.sock --state /tmp/natmap_state.json --socket-group root >/tmp/natmap.log 2>&1 &
NATMAP_PID=$!
for i in $(seq 1 40); do [ -S /tmp/natmap.sock ] && break; sleep 0.2; done
if ! [ -S /tmp/natmap.sock ]; then echo "FAIL: natmap socket never appeared" >&2; cat /tmp/natmap.log; exit 1; fi
"#
    )
}
