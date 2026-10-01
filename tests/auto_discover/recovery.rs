//! Docker integration tests for daemon recovery from an invalid config.

use super::*;

#[test]
fn invalid_yaml_config_daemon_warns_not_crash() {
    let script = format!(
        r#"{infra}
echo "invalid: yaml: {{broken" > /tmp/bad-config.yaml

lab-ops auto-discover sync /tmp/bad-config.yaml 2>/tmp/sync-err.log && {{ echo "FAIL: sync should have failed"; exit 1; }} || true

echo "PASS: sync correctly rejected invalid YAML"
"#,
        infra = infra_setup(false),
    );
    run(&script);
}

#[test]
fn restart_auto_discover_picks_up_missed_containers() {
    let cname = "it-restart-ad";
    let services_yaml = r#"
services:
  it-svc-restart:
    type: docker
    match:
      project: it-svc-restart
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-restart.test.local"#;
    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-restart" nginx:alpine
{before}

{daemon}
{after}
echo "PASS: container started while the daemon was down is picked up on restart"
docker rm -f {cname} 2>/dev/null || true
"#,
        setup = base_setup(services_yaml, true),
        // Nothing may register before the daemon exists, so poll that the
        // container is up and Consul is answering, then assert the absence.
        before = poll_until(
            "docker inspect -f '{{.State.Running}}' it-restart-ad 2>/dev/null | grep true",
            "RUNNING",
            20,
            "container never reached the running state",
        ) + &assert_absent("it-svc-restart", "registered before the daemon started"),
        daemon = start_daemon("--no-forwarding"),
        after = wait_for_consul_service("it-svc-restart", 30),
        cname = cname,
    );
    run(&script);
}

#[test]
fn restart_natmap_new_container_registered_after_recovery() {
    let services_yaml = r#"
services:
  it-svc-nmrestart:
    type: docker
    match:
      project: it-svc-nmrestart
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-nmrestart.test.local"#;
    let script = format!(
        r#"{setup}
{daemon}
docker run -d --name it-nmrestart-a -l "com.docker.compose.project=it-svc-nmrestart" nginx:alpine
{first}

{natmap_down}
docker run -d --name it-nmrestart-b -l "com.docker.compose.project=it-svc-nmrestart" nginx:alpine
{down}

rm -f /tmp/natmap_state.json
lab-ops natmap daemon --socket /tmp/natmap.sock --state /tmp/natmap_state.json --socket-group root >/tmp/natmap2.log 2>&1 &
NATMAP_PID=$!
for i in $(seq 1 40); do [ -S /tmp/natmap.sock ] && break; sleep 0.2; done
if ! [ -S /tmp/natmap.sock ]; then echo "FAIL: natmap did not come back" >&2; cat /tmp/natmap2.log; exit 1; fi

docker rm -f it-nmrestart-b 2>/dev/null || true
docker run -d --name it-nmrestart-c -l "com.docker.compose.project=it-svc-nmrestart" nginx:alpine
{recovered}

echo "PASS: new container registered after natmap recovery"
docker rm -f it-nmrestart-a it-nmrestart-c 2>/dev/null || true
"#,
        setup = base_setup(services_yaml, true),
        daemon = start_daemon("--no-forwarding"),
        first = wait_for_consul_service("it-svc-nmrestart", 30),
        // Container A is still registered, so the bound is one: B must not add
        // a second entry while natmap is down.
        natmap_down = stop_natmap(),
        down = poll_until(
            "docker inspect -f '{{.State.Running}}' it-nmrestart-b 2>/dev/null | grep true",
            "RUNNING",
            20,
            "second container never reached the running state",
        ) + &assert_count_at_most("it-svc-nmrestart", 1, "while natmap was down"),
        recovered = wait_for_consul_service("it-svc-nmrestart", 30),
    );
    run(&script);
}

#[test]
fn add_service_to_config_picked_up_on_sync() {
    let first_yaml = r#"
services:
  it-svc-cfg-a:
    type: docker
    match:
      project: it-svc-cfg-a
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-cfg-a.test.local"#;
    let second_yaml = r#"
services:
  it-svc-cfg-a:
    type: docker
    match:
      project: it-svc-cfg-a
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-cfg-a.test.local
  it-svc-cfg-b:
    type: docker
    match:
      project: it-svc-cfg-b
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-cfg-b.test.local"#;
    let script = format!(
        r#"{setup}
docker run -d --name it-cfg-a -l "com.docker.compose.project=it-svc-cfg-a" nginx:alpine
{first}

{rewrite}
docker run -d --name it-cfg-b -l "com.docker.compose.project=it-svc-cfg-b" nginx:alpine
{second}

echo "PASS: new service registered after config change"
docker rm -f it-cfg-a it-cfg-b 2>/dev/null || true
"#,
        setup = new_format_setup(first_yaml, ""),
        first = wait_for_consul_service("it-svc-cfg-a", 30),
        rewrite = write_discovery_config(&format!("node:\n  name: int-test-node\n{second_yaml}")),
        second = wait_for_consul_service("it-svc-cfg-b", 30),
    );
    run(&script);
}

/// `remove_service_from_config_stale_deregistered` and
/// `remove_all_services_clean_slate` were the same test under two names: both
/// emptied `services:`, ran `sync`, and asserted nothing stale remained. Merged
/// here as the config-emptied case.
#[test]
fn remove_all_services_clean_slate() {
    let services_yaml = r#"
services:
  it-cfg-all-svc:
    type: docker
    match:
      project: it-cfg-all-svc
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-cfg-all.test.local"#;
    let script = format!(
        r#"{setup}
docker run -d --name it-cfg-all -l "com.docker.compose.project=it-cfg-all-svc" nginx:alpine
{first}

{stop}
{empty}
{sync}
{gone}

echo "PASS: all services deregistered after emptying the config"
docker rm -f it-cfg-all 2>/dev/null || true
"#,
        setup = new_format_setup(services_yaml, ""),
        first = wait_for_consul_service("it-cfg-all-svc", 30),
        stop = stop_daemon(),
        empty = write_discovery_config("node:\n  name: int-test-node\nservices: {}"),
        sync = sync_once(),
        gone = assert_node_registrations_gone(),
    );
    run(&script);
}

#[test]
fn change_bind_ip_service_reregisters() {
    let first_yaml = r#"
services:
  it-cfg-ip-svc:
    type: docker
    match:
      project: it-cfg-ip-svc
    bind_ip: 127.0.0.1
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-cfg-ip.test.local"#;
    let second_yaml = r#"
services:
  it-cfg-ip-svc:
    type: docker
    match:
      project: it-cfg-ip-svc
    bind_ip: 10.99.99.1
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-cfg-ip.test.local"#;
    let script = format!(
        r#"{setup}
docker run -d --name it-cfg-ip -l "com.docker.compose.project=it-cfg-ip-svc" nginx:alpine
{first}

{stop}
{rewrite}
{sync}
{second}

echo "PASS: bind_ip updated to $ADDR2"
docker rm -f it-cfg-ip 2>/dev/null || true
"#,
        setup = new_format_setup(first_yaml, ""),
        stop = stop_daemon(),
        rewrite = write_discovery_config(&format!("node:\n  name: int-test-node\n{second_yaml}")),
        sync = sync_once(),
        first = poll_until(
            "curl -sf $CONSUL_HTTP_ADDR/v1/agent/services 2>/dev/null | jq -r 'to_entries[] | select(.value.Service == \"it-cfg-ip-svc\") | .value.Address' 2>/dev/null",
            "ADDR1",
            30,
            "it-cfg-ip-svc never registered",
        ) + r#"
if [ "$ADDR1" != "127.0.0.1" ]; then echo "FAIL: expected Address=127.0.0.1, got $ADDR1" >&2; exit 1; fi
"#,
        second = poll_until(
            "curl -sf $CONSUL_HTTP_ADDR/v1/agent/services 2>/dev/null | jq -r 'to_entries[] | select(.value.Service == \"it-cfg-ip-svc\") | .value.Address' 2>/dev/null",
            "ADDR2",
            30,
            "it-cfg-ip-svc never re-registered after the bind_ip change",
        ) + r#"
if [ "$ADDR2" != "10.99.99.1" ]; then echo "FAIL: expected Address=10.99.99.1 after change, got $ADDR2" >&2; exit 1; fi
"#,
    );
    run(&script);
}

#[test]
fn large_config_many_services() {
    let mut yaml_services = String::new();
    let mut cnames = Vec::new();
    for i in 0..5 {
        let project = format!("it-large-{i}");
        yaml_services.push_str(&format!(
            "  {project}:\n    type: docker\n    match:\n      project: {project}\n    rproxylocal:\n      - port: 80\n        template: HTTP_PROXY\n        domains:\n          - {project}.test.local\n"
        ));
        cnames.push(project);
    }
    let services_yaml = format!("\nservices:\n{yaml_services}");
    let script = format!(
        r#"{setup}
for cn in {cnames_list}; do
    docker run -d --name "$cn" -l "com.docker.compose.project=$cn" nginx:alpine
done
{all}

echo "PASS: all 5 services registered"
docker rm -f {cnames_list} 2>/dev/null || true
"#,
        setup = new_format_setup(&services_yaml, ""),
        all = poll_until(
            "curl -sf $CONSUL_HTTP_ADDR/v1/agent/services 2>/dev/null | jq '[to_entries[] | select(.value.Service | startswith(\"it-large-\"))] | if length >= 5 then length else empty end' 2>/dev/null",
            "COUNT",
            40,
            "fewer than 5 services registered",
        ),
        cnames_list = cnames.join(" "),
    );
    run(&script);
}
