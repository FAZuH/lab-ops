//! Docker integration tests for the daemon's startup race and fail-closed sweep.

use super::*;

#[test]
fn full_sync_failure_does_not_deregister_services() {
    let cname = "it-startup-race";
    let services_yaml = r#"
services:
  it-svc-startup:
    type: docker
    match:
      project: it-svc-startup
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-startup.test.local"#;
    let script = format!(
        r#"{setup}
{daemon}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-startup" nginx:alpine
{registered}

SVC_BEFORE=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-startup") | .value.Port // empty')
if [ -z "$SVC_BEFORE" ]; then echo "FAIL: service not registered before race" >&2; cat /tmp/discovery.log; exit 1; fi
echo "Service registered before failed sync: port=$SVC_BEFORE"

# Reproduce the startup race: stop natmap and remove its socket so every
# add_docker_mapping call fails, then run a one-shot sync.
{stop}
{natmap_down}

if lab-ops auto-discover sync /tmp/discovery.yaml >/tmp/sync.log 2>&1; then
    echo "FAIL: sync should have exited non-zero (all natmap mappings errored)" >&2
    cat /tmp/sync.log
    exit 1
fi
echo "Sync correctly exited non-zero (startup retry would engage)"

SVC_AFTER=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-startup") | .value.Port // empty')
if [ -z "$SVC_AFTER" ]; then
    echo "FAIL: previously-registered service was deregistered by failed sync" >&2
    cat /tmp/sync.log
    exit 1
fi

echo "PASS: failed sync preserved existing registration (port=$SVC_AFTER)"
docker rm -f {cname} 2>/dev/null || true
kill %1 %2 2>/dev/null || true
sleep 1
"#,
        setup = base_setup(services_yaml, true),
        daemon = start_daemon("--no-forwarding"),
        registered = wait_for_consul_service("it-svc-startup", 30),
        stop = stop_daemon(),
        natmap_down = stop_natmap() + "rm -f /tmp/natmap.sock\n",
        cname = cname,
    );
    let out = run(&script);
    assert_pass(&out, "startup race — all mappings fail on sync");
}
