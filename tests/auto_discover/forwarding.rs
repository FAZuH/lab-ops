use super::*;

#[test]
fn forwarding_sync_applies_dnat_rules() {
    let script = r#"
set -e
NATMAP_SOCKET=/tmp/natmap.sock
CONSUL_HTTP_ADDR=http://127.0.0.1:8500

consul agent -dev -http-port=8500 >/tmp/consul.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: consul died"; cat /tmp/consul.log; exit 1; }

rm -f /tmp/natmap_state.json
lab-ops natmap daemon --socket $NATMAP_SOCKET --state /tmp/natmap_state.json --socket-group root >/tmp/natmap.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: natmap died"; cat /tmp/natmap.log; exit 1; }

curl -sf -X PUT "$CONSUL_HTTP_ADDR/v1/agent/service/register" \
    -d '{ "ID": "fwd-test-svc", "Name": "fwd-svc", "Address": "10.0.0.99", "Port": 36000, "Meta": { "forwarding": "true", "ext_ip": "203.0.113.50", "ext_ports": "36000" } }'

lab-ops auto-discover forwarding-sync $CONSUL_HTTP_ADDR >/tmp/fwd-sync.log 2>&1 || true

if ! iptables-save -t nat | grep -q "203.0.113.50.*10.0.0.99"; then echo "FAIL: DNAT rule not found" >&2; exit 1; fi

echo "PASS: forwarding-sync created DNAT rules"
kill %1 %2 2>/dev/null || true
sleep 1
"#.to_string();
    let out = run(&script);
    assert_pass(&out, "Phase 7 — forwarding sync DNAT");
}

#[test]
fn forwarding_sync_no_duplicate_rules() {
    let script = r#"
set -e
NATMAP_SOCKET=/tmp/natmap.sock
CONSUL_HTTP_ADDR=http://127.0.0.1:8500

consul agent -dev -http-port=8500 >/tmp/consul.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: consul died"; cat /tmp/consul.log; exit 1; }

rm -f /tmp/natmap_state.json
lab-ops natmap daemon --socket $NATMAP_SOCKET --state /tmp/natmap_state.json --socket-group root >/tmp/natmap.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: natmap died"; cat /tmp/natmap.log; exit 1; }

curl -sf -X PUT "$CONSUL_HTTP_ADDR/v1/agent/service/register" \
    -d '{ "ID": "fwd-dup-svc", "Name": "fwd-dup", "Address": "10.0.0.99", "Port": 36003, "Meta": { "forwarding": "true", "ext_ip": "203.0.113.51", "ext_ports": "36003" } }'

# Run forwarding-sync 3 times — should produce only 1 DNAT rule
lab-ops auto-discover forwarding-sync $CONSUL_HTTP_ADDR >/tmp/fwd1.log 2>&1 || true
lab-ops auto-discover forwarding-sync $CONSUL_HTTP_ADDR >/tmp/fwd2.log 2>&1 || true
lab-ops auto-discover forwarding-sync $CONSUL_HTTP_ADDR >/tmp/fwd3.log 2>&1 || true

COUNT=$(iptables-save -t nat | grep -c "203.0.113.51.*10.0.0.99" || true)
if [ "$COUNT" -ne 1 ]; then echo "FAIL: expected 1 DNAT rule, got $COUNT" >&2; exit 1; fi

echo "PASS: no duplicate DNAT rules after multiple syncs"
kill %1 %2 2>/dev/null || true
sleep 1
"#.to_string();
    let out = run(&script);
    assert_pass(&out, "forwarding_sync_no_duplicate_rules");
}

#[test]
fn forwarding_sync_removes_stale_rules() {
    let script = r#"
set -e
NATMAP_SOCKET=/tmp/natmap.sock
CONSUL_HTTP_ADDR=http://127.0.0.1:8500

consul agent -dev -http-port=8500 >/tmp/consul.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: consul died"; cat /tmp/consul.log; exit 1; }

rm -f /tmp/natmap_state.json
lab-ops natmap daemon --socket $NATMAP_SOCKET --state /tmp/natmap_state.json --socket-group root >/tmp/natmap.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: natmap died"; cat /tmp/natmap.log; exit 1; }

curl -sf -X PUT "$CONSUL_HTTP_ADDR/v1/agent/service/register" \
    -d '{ "ID": "fwd-stale-svc", "Name": "fwd-stale", "Address": "10.0.0.99", "Port": 36002, "Meta": { "forwarding": "true", "ext_ip": "203.0.113.50", "ext_ports": "36002" } }'

lab-ops auto-discover forwarding-sync $CONSUL_HTTP_ADDR >/tmp/fwd1.log 2>&1

curl -sf -X PUT "$CONSUL_HTTP_ADDR/v1/agent/service/deregister/fwd-stale-svc" >/dev/null 2>&1

lab-ops auto-discover forwarding-sync $CONSUL_HTTP_ADDR >/tmp/fwd2.log 2>&1 || true

COUNT=$(iptables-save -t nat | grep -c "203.0.113.50.*10.0.0.99" || true)
if [ "$COUNT" -ne 0 ]; then echo "FAIL: expected 0 stale DNAT rules, got $COUNT" >&2; exit 1; fi

echo "PASS: stale DNAT rules removed"
kill %1 %2 2>/dev/null || true
sleep 1
"#.to_string();
    let out = run(&script);
    assert_pass(&out, "Phase 7 — stale rule cleanup");
}

#[test]
fn no_forwarding_services_sync_noop() {
    let script = r#"
set -e
NATMAP_SOCKET=/tmp/natmap.sock
CONSUL_HTTP_ADDR=http://127.0.0.1:8500

consul agent -dev -http-port=8500 >/tmp/consul.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: consul died"; cat /tmp/consul.log; exit 1; }

rm -f /tmp/natmap_state.json
lab-ops natmap daemon --socket $NATMAP_SOCKET --state /tmp/natmap_state.json --socket-group root >/tmp/natmap.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: natmap died"; cat /tmp/natmap.log; exit 1; }

lab-ops auto-discover forwarding-sync $CONSUL_HTTP_ADDR >/tmp/fwd.log 2>&1

echo "PASS: forwarding-sync noop with no services"
kill %1 %2 2>/dev/null || true
sleep 1
"#.to_string();
    let out = run(&script);
    assert_pass(&out, "Phase 7 — noop forwarding sync");
}

#[test]
fn forwarding_group_multiple_ports() {
    let script = r#"
set -e
NATMAP_SOCKET=/tmp/natmap.sock
CONSUL_HTTP_ADDR=http://127.0.0.1:8500

consul agent -dev -http-port=8500 >/tmp/consul.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: consul died"; cat /tmp/consul.log; exit 1; }

rm -f /tmp/natmap_state.json
lab-ops natmap daemon --socket $NATMAP_SOCKET --state /tmp/natmap_state.json --socket-group root >/tmp/natmap.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: natmap died"; cat /tmp/natmap.log; exit 1; }

curl -sf -X PUT "$CONSUL_HTTP_ADDR/v1/agent/service/register" \
    -d '{ "ID": "fwd-multi", "Name": "fwd-multi", "Address": "10.0.0.99", "Port": 36005, "Meta": { "forwarding": "true", "ext_ip": "203.0.113.60", "ext_ports": "36005,36006,36007" } }'

lab-ops auto-discover forwarding-sync $CONSUL_HTTP_ADDR >/tmp/fwd.log 2>&1 || true

if ! iptables-save -t nat | grep -q "203.0.113.60"; then echo "FAIL: no DNAT rules for multi-port" >&2; exit 1; fi

echo "PASS: forwarding sync with multiple ports created DNAT rules"
kill %1 %2 2>/dev/null || true
sleep 1
"#.to_string();
    let out = run(&script);
    assert_pass(&out, "Phase 7 — multiple ports forwarding");
}

#[test]
fn forwarding_static_port() {
    let cname = "it-fwd";
    let services_yaml = r#"
services:
  it-svc-d:
    type: docker
    match:
      project: it-svc-d
    forwardremote:
    - port: 80
      ext_ip: 203.0.113.43
      ext_ports:
      - 36000
      proto: tcp"#;

    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-d" nginx:alpine
sleep 4

SVC=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq 'to_entries[] | select(.value.Service == "it-svc-d") | .value')
PORT=$(echo "$SVC" | jq -r '.Port')
if [ "$PORT" != "36000" ]; then echo "FAIL: expected static port 36000, got $PORT" >&2; exit 1; fi

FORWARDING=$(echo "$SVC" | jq -r '.Meta.forwarding')
if [ "$FORWARDING" != "true" ]; then echo "FAIL: missing forwarding meta" >&2; exit 1; fi

EXT_IP=$(echo "$SVC" | jq -r '.Meta.ext_ip')
if [ "$EXT_IP" != "203.0.113.43" ]; then echo "FAIL: incorrect ext_ip: $EXT_IP" >&2; exit 1; fi

EXT_PORTS=$(echo "$SVC" | jq -r '.Meta.ext_ports')
if [ "$EXT_PORTS" != "36000" ]; then echo "FAIL: incorrect ext_ports: $EXT_PORTS" >&2; exit 1; fi

echo "PASS: static port 36000 with forwarding meta"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );

    let out = run(&script);
    assert_pass(&out, "Test D — forwarding static port");
}

#[test]
fn forwarding_hairpin_meta() {
    let cname = "it-hairpin";
    let services_yaml = r#"
services:
  it-svc-e:
    type: docker
    match:
      project: it-svc-e
    forwardremote:
    - port: 80
      ext_ip: 203.0.113.43
      ext_ports:
      - 36001
      proto: tcp
      hairpin: true"#;

    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-e" nginx:alpine
sleep 4

SVC=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq 'to_entries[] | select(.value.Service == "it-svc-e") | .value')
PORT=$(echo "$SVC" | jq -r '.Port')
if [ "$PORT" != "36001" ]; then echo "FAIL: expected static port 36001, got $PORT" >&2; exit 1; fi

HAIRPIN=$(echo "$SVC" | jq -r '.Meta.hairpin')
if [ "$HAIRPIN" != "true" ]; then echo "FAIL: missing hairpin meta" >&2; exit 1; fi

echo "PASS: static port 36001 with hairpin meta"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );

    let out = run(&script);
    assert_pass(&out, "Test E — forwarding hairpin");
}

#[test]
fn forwarding_sync_preserve_src_ip_creates_lan_hairpin() {
    let script = r#"
set -e
NATMAP_SOCKET=/tmp/natmap.sock
CONSUL_HTTP_ADDR=http://127.0.0.1:8500

consul agent -dev -http-port=8500 >/tmp/consul.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: consul died"; cat /tmp/consul.log; exit 1; }

rm -f /tmp/natmap_state.json
lab-ops natmap daemon --socket $NATMAP_SOCKET --state /tmp/natmap_state.json --socket-group root >/tmp/natmap.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: natmap died"; cat /tmp/natmap.log; exit 1; }

curl -sf -X PUT "$CONSUL_HTTP_ADDR/v1/agent/service/register" \
    -d '{ "ID": "fwd-hp-ps-svc", "Name": "fwd-hp-ps", "Address": "10.0.0.100", "Port": 36010, "Meta": { "forwarding": "true", "ext_ip": "203.0.113.52", "ext_ports": "36010", "hairpin": "true", "preserve_src_ip": "true" } }'

lab-ops auto-discover forwarding-sync $CONSUL_HTTP_ADDR >/tmp/fwd.log 2>&1 || true

# DNAT rule must exist
if ! iptables-save -t nat | grep -q "203.0.113.52.*10.0.0.100"; then echo "FAIL: DNAT rule not found" >&2; exit 1; fi

# Hairpin POSTROUTING MASQUERADE must exist (LAN-limited for preserve_src_ip)
if ! iptables-save -t nat | grep -q "A POSTROUTING.*10.0.0.100.*MASQUERADE"; then
    echo "FAIL: LAN hairpin MASQUERADE not found" >&2
    iptables-save -t nat | grep "10.0.0.100" >&2
    exit 1
fi

# Must NOT use global 0.0.0.0/0 source (must be LAN-limited)
if iptables-save -t nat | grep -q "A POSTROUTING -s 0.0.0.0/0.*10.0.0.100.*MASQUERADE"; then
    echo "FAIL: hairpin MASQUERADE uses global source instead of LAN-limited" >&2
    iptables-save -t nat | grep "10.0.0.100" >&2
    exit 1
fi

echo "PASS: preserve_src_ip creates LAN-limited hairpin MASQUERADE"
kill %1 %2 2>/dev/null || true
sleep 1
"#.to_string();
    let out = run(&script);
    assert_pass(&out, "forwarding_sync_preserve_src_ip_creates_lan_hairpin");
}

#[test]
fn forwarding_sync_hairpin_creates_masquerade() {
    let script = r#"
set -e
NATMAP_SOCKET=/tmp/natmap.sock
CONSUL_HTTP_ADDR=http://127.0.0.1:8500

consul agent -dev -http-port=8500 >/tmp/consul.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: consul died"; cat /tmp/consul.log; exit 1; }

rm -f /tmp/natmap_state.json
lab-ops natmap daemon --socket $NATMAP_SOCKET --state /tmp/natmap_state.json --socket-group root >/tmp/natmap.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: natmap died"; cat /tmp/natmap.log; exit 1; }

curl -sf -X PUT "$CONSUL_HTTP_ADDR/v1/agent/service/register" \
    -d '{ "ID": "fwd-hp-svc", "Name": "fwd-hp", "Address": "10.0.0.101", "Port": 36011, "Meta": { "forwarding": "true", "ext_ip": "203.0.113.53", "ext_ports": "36011", "hairpin": "true" } }'

lab-ops auto-discover forwarding-sync $CONSUL_HTTP_ADDR >/tmp/fwd.log 2>&1 || true

# DNAT rule must exist
if ! iptables-save -t nat | grep -q "203.0.113.53.*10.0.0.101"; then echo "FAIL: DNAT rule not found" >&2; exit 1; fi

# Hairpin POSTROUTING MASQUERADE must exist
if ! iptables-save -t nat | grep -q "A POSTROUTING.*10.0.0.101.*MASQUERADE"; then
    echo "FAIL: hairpin MASQUERADE not found" >&2
    iptables-save -t nat | grep "10.0.0.101" >&2
    exit 1
fi

echo "PASS: hairpin creates POSTROUTING MASQUERADE"
kill %1 %2 2>/dev/null || true
sleep 1
"#.to_string();
    let out = run(&script);
    assert_pass(&out, "forwarding_sync_hairpin_creates_masquerade");
}
