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
        r#"
set -e
export NATMAP_SOCKET=/tmp/natmap.sock
export CONSUL_HTTP_ADDR=http://127.0.0.1:8500

consul agent -dev -http-port=8500 -pid-file=/tmp/consul.pid >/tmp/consul.log 2>&1 &
sleep 2
if ! kill -0 $! 2>/dev/null; then echo "FAIL: consul died" >&2; cat /tmp/consul.log; exit 1; fi

ip link add dummy0 type dummy 2>/dev/null || true
ip addr add 10.99.99.1/24 dev dummy0 2>/dev/null || true
ip link set dummy0 up

rm -f /tmp/natmap_state.json
lab-ops natmap daemon --socket /tmp/natmap.sock --state /tmp/natmap_state.json --socket-group root >/tmp/natmap.log 2>&1 &
NATMAP_PID=$!
sleep 2
if ! kill -0 $NATMAP_PID 2>/dev/null; then echo "FAIL: natmap daemon died" >&2; cat /tmp/natmap.log; exit 1; fi

cat > /tmp/discovery.yaml <<'YAMLEOF'
node:
  name: int-test-node
{services_yaml}
YAMLEOF

lab-ops auto-discover daemon /tmp/discovery.yaml \
    --state-dir /tmp/state \
    --no-forwarding \
    --consul-addr http://127.0.0.1:8500 \
    >/tmp/discovery.log 2>&1 &
sleep 2
if ! kill -0 $! 2>/dev/null; then echo "FAIL: auto-discover daemon died" >&2; cat /tmp/discovery.log; exit 1; fi

docker run -d --name {cname} -l "com.docker.compose.project=it-svc-startup" nginx:alpine
sleep 4

SVC_BEFORE=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-startup") | .value.Port // empty')
if [ -z "$SVC_BEFORE" ]; then echo "FAIL: service not registered before race" >&2; cat /tmp/discovery.log; exit 1; fi
echo "Service registered before failed sync: port=$SVC_BEFORE"

# Reproduce the startup race: stop natmap and remove its socket so every
# add_docker_mapping call fails, then run a one-shot sync.
kill %3 2>/dev/null || true
kill $NATMAP_PID 2>/dev/null || true
sleep 1
rm -f /tmp/natmap.sock

if lab-ops auto-discover sync /tmp/discovery.yaml --state-dir /tmp/state >/tmp/sync.log 2>&1; then
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
    );
    let out = run(&script);
    assert_pass(&out, "startup race — all mappings fail on sync");
}
