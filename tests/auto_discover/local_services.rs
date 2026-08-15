use super::*;

#[test]
fn local_service() {
    let services_yaml = r#"
services:
  it-local-app:
    type: local
    address: 10.99.99.99
    rproxylocal:
    - port: 3000
      template: HTTP_PROXY
      domains:
      - app.local.test"#;

    let script = format!(
        r#"{setup}
{wait}
PORT=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-local-app") | .value.Port')
if [ -z "$PORT" ] || [ "$PORT" = "null" ]; then echo "FAIL: not registered with Consul" >&2; cat /tmp/discovery.log; exit 1; fi
if [ "$PORT" != "3000" ]; then echo "FAIL: expected port 3000, got $PORT" >&2; exit 1; fi

ADDR=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-local-app") | .value.Address')
if [ "$ADDR" != "10.99.99.99" ]; then echo "FAIL: expected address 10.99.99.99, got $ADDR" >&2; exit 1; fi

echo "PASS: local service registered at $ADDR:$PORT, NAT bypassed"
kill %3 %2 %1 2>/dev/null || true
sleep 1
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        wait = wait_for_consul_service("it-local-app", 15),
    );
    let out = run(&script);
    assert_pass(&out, "local_service");
}

#[test]
fn local_forwarding_remote() {
    let services_yaml = r#"
services:
  it-local-fwd:
    type: local
    address: 10.99.99.99
    forwardremote:
    - port: 4000
      ext_ip: 203.0.113.43
      ext_ports:
      - 40000
      proto: tcp"#;

    let script = format!(
        r#"{setup}
{wait}
PORT=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-local-fwd") | .value.Port')
if [ -z "$PORT" ] || [ "$PORT" = "null" ]; then echo "FAIL: not registered with Consul" >&2; exit 1; fi
if [ "$PORT" != "40000" ]; then echo "FAIL: expected static port 40000, got $PORT" >&2; exit 1; fi

ADDR=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-local-fwd") | .value.Address')
if [ "$ADDR" != "10.99.99.99" ]; then echo "FAIL: expected address 10.99.99.99, got $ADDR" >&2; exit 1; fi

SVC=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq 'to_entries[] | select(.value.Service == "it-local-fwd") | .value.Meta')
FORWARDING=$(echo "$SVC" | jq -r '.forwarding')
if [ "$FORWARDING" != "true" ]; then echo "FAIL: missing forwarding meta" >&2; exit 1; fi

EXT_IP=$(echo "$SVC" | jq -r '.ext_ip')
if [ "$EXT_IP" != "203.0.113.43" ]; then echo "FAIL: missing ext_ip meta" >&2; exit 1; fi

EXT_PORTS=$(echo "$SVC" | jq -r '.ext_ports')
if [ "$EXT_PORTS" != "40000" ]; then echo "FAIL: missing ext_ports meta" >&2; exit 1; fi

echo "PASS: local forwarding remote at 10.99.99.99:40000 with forwarding meta"
kill %3 %2 %1 2>/dev/null || true
sleep 1
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        wait = wait_for_consul_service("it-local-fwd", 15),
    );
    let out = run(&script);
    assert_pass(&out, "local_forwarding_remote");
}

#[test]
fn docker_reachability() {
    let cname = "it-reach";
    let services_yaml = r#"
services:
  it-svc-reach:
    type: docker
    match:
      project: it-svc-reach
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - reach.test.local"#;

    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-reach" nginx:alpine
sleep 4

PORT=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-reach") | .value.Port')
if [ -z "$PORT" ] || [ "$PORT" = "null" ]; then echo "FAIL: not registered with Consul" >&2; cat /tmp/discovery.log; exit 1; fi

# Verify iptables DNAT rule exists
iptables -t nat -S NATMAP | grep -q "to-destination.*:80" || \
    {{ echo "FAIL: no DNAT rule for port $PORT" >&2; iptables -t nat -S NATMAP >&2; exit 1; }}

# Verify FORWARD ACCEPT rule exists
iptables -t filter -S NATMAP 2>/dev/null | grep -q "ACCEPT" || \
    {{ echo "FAIL: no FORWARD ACCEPT rule" >&2; iptables -t filter -S NATMAP >&2; exit 1; }}

# Verify OUTPUT DNAT rule exists for localhost traffic
iptables -t nat -S OUTPUT 2>/dev/null | grep -q "to-destination.*:80" || \
    {{ echo "FAIL: no OUTPUT DNAT rule" >&2; iptables -t nat -S OUTPUT >&2; exit 1; }}

# Verify container is running and serving traffic directly
docker exec {cname} wget -q -O - http://localhost:80/ 2>/dev/null | grep -qi nginx || \
    {{ echo "FAIL: nginx container not serving traffic" >&2; docker exec {cname} wget -O - http://localhost:80/ 2>&1 || true; exit 1; }}

echo "PASS: reachable, DNAT rules verified, container serving"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );
    let out = run(&script);
    assert_pass(&out, "docker_reachability");
}

#[test]
fn docker_rproxy_and_forwarding() {
    let cname = "it-combo";
    let services_yaml = r#"
services:
  it-svc-combo:
    type: docker
    match:
      project: it-svc-combo
    rproxylocal:
      - port: 80
        template: HTTP_PROXY
        domains:
          - combo.test.local
    forwardremote:
      - port: 80
        ext_ip: 203.0.113.43
        ext_ports:
          - 36000
        proto: tcp"#;

    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-combo" nginx:alpine
sleep 4

# Expect 2 Consul entries: 1 forwardremote + 1 rproxylocal (no merging)
COUNT=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq '[to_entries[] | select(.value.Service == "it-svc-combo")] | length')
if [ "$COUNT" != "2" ]; then echo "FAIL: expected 2 Consul entries, got $COUNT" >&2; curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq '[to_entries[] | select(.value.Service == "it-svc-combo") | .key, .value.Port, .value.Meta.forwarding // "n/a"]' >&2; exit 1; fi

# Find the forwardremote entry (static port 36000)
FWD_SVC=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq 'to_entries[] | select(.value.Service == "it-svc-combo" and .value.Port == 36000) | .value')
if [ -z "$FWD_SVC" ]; then echo "FAIL: forwardremote entry at port 36000 not found" >&2; exit 1; fi

FWD_META=$(echo "$FWD_SVC" | jq '.Meta')
FORWARDING=$(echo "$FWD_META" | jq -r '.forwarding')
if [ "$FORWARDING" != "true" ]; then echo "FAIL: missing forwarding meta" >&2; exit 1; fi

EXT_IP=$(echo "$FWD_META" | jq -r '.ext_ip')
if [ "$EXT_IP" != "203.0.113.43" ]; then echo "FAIL: missing ext_ip meta" >&2; exit 1; fi

# ForwardRemote should NOT have a template
TEMPLATE=$(echo "$FWD_META" | jq -r '.template // "empty"')
if [ "$TEMPLATE" != "empty" ]; then echo "FAIL: forwardremote should not have template, got $TEMPLATE" >&2; exit 1; fi

# Find the rproxylocal entry (ephemeral port)
RPROXY_SVC=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq 'to_entries[] | select(.value.Service == "it-svc-combo" and .value.Port != 36000) | .value')
if [ -z "$RPROXY_SVC" ]; then echo "FAIL: rproxylocal entry not found" >&2; exit 1; fi

RPROXY_META=$(echo "$RPROXY_SVC" | jq '.Meta')
RPROXY_TEMPLATE=$(echo "$RPROXY_META" | jq -r '.template')
if [ "$RPROXY_TEMPLATE" != "HTTP_PROXY" ]; then echo "FAIL: expected template HTTP_PROXY for rproxy, got $RPROXY_TEMPLATE" >&2; exit 1; fi

PORT=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-combo") | .value.Port')
echo "PASS: forwardremote + rproxylocal separate entries"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );

    let out = run(&script);
    assert_pass(&out, "docker_rproxy_and_forwarding");
}
