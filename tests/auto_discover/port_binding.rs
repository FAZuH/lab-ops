use super::*;

#[test]
fn docker_forwarding_local_bind_port() {
    let cname = "it-fwd-local";
    let services_yaml = r#"
services:
  it-svc-fwd-local:
    type: docker
    match:
      project: it-svc-fwd-local
    forwardlocal:
      - port: 80
        bind_port: 36000
"#;

    let script = format!(
        r#"{setup}
docker rm -f {cname} 2>/dev/null || true
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-fwd-local" nginx:alpine
sleep 4

SVC=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq 'to_entries[] | select(.value.Service == "it-svc-fwd-local") | .value')
PORT=$(echo "$SVC" | jq -r '.Port')
if [ "$PORT" != "36000" ]; then echo "FAIL: expected static port 36000, got $PORT" >&2; exit 1; fi

FORWARDING=$(echo "$SVC" | jq -r '.Meta.forwarding')
if [ "$FORWARDING" != "true" ]; then echo "FAIL: missing forwarding meta" >&2; exit 1; fi

FWD_TYPE=$(echo "$SVC" | jq -r '.Meta.forwarding_type')
if [ "$FWD_TYPE" != "local" ]; then echo "FAIL: expected forwarding_type=local, got $FWD_TYPE" >&2; exit 1; fi

echo "PASS: forwarding local bind_port=36000 with forwarding_type=local"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );

    let out = run(&script);
    assert_pass(&out, "docker_forwarding_local_bind_port");
}

#[test]
fn docker_forwarding_local_with_template() {
    let cname = "it-fwd-local-tpl";
    let services_yaml = r#"
services:
  it-svc-fwd-local-tpl:
    type: docker
    match:
      project: it-svc-fwd-local-tpl
    rproxylocal:
      - port: 80
        template: HTTP_PROXY
        domains:
          - fwd-local-tpl.test.local
    forwardlocal:
      - port: 80
        bind_port: 36001
"#;

    let script = format!(
        r#"{setup}
docker rm -f {cname} 2>/dev/null || true
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-fwd-local-tpl" nginx:alpine
sleep 4

# Expect 2 Consul entries: 1 forwardlocal + 1 rproxylocal (no merging)
COUNT=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq '[to_entries[] | select(.value.Service == "it-svc-fwd-local-tpl")] | length')
if [ "$COUNT" != "2" ]; then echo "FAIL: expected 2 Consul entries, got $COUNT" >&2; exit 1; fi

# Find the forwardlocal entry (static port 36001)
FWD_SVC=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq 'to_entries[] | select(.value.Service == "it-svc-fwd-local-tpl" and .value.Port == 36001) | .value')
if [ -z "$FWD_SVC" ]; then echo "FAIL: forwardlocal entry at port 36001 not found" >&2; exit 1; fi

FWD_META=$(echo "$FWD_SVC" | jq '.Meta')
FWD_FORWARDING=$(echo "$FWD_META" | jq -r '.forwarding')
if [ "$FWD_FORWARDING" != "true" ]; then echo "FAIL: missing forwarding meta" >&2; exit 1; fi

FWD_TYPE=$(echo "$FWD_META" | jq -r '.forwarding_type')
if [ "$FWD_TYPE" != "local" ]; then echo "FAIL: expected forwarding_type=local, got $FWD_TYPE" >&2; exit 1; fi

# ForwardLocal should NOT have a template
FWD_TEMPLATE=$(echo "$FWD_META" | jq -r '.template // "empty"')
if [ "$FWD_TEMPLATE" != "empty" ]; then echo "FAIL: forwardlocal should not have template, got $FWD_TEMPLATE" >&2; exit 1; fi

# Find the rproxylocal entry (ephemeral port)
RPROXY_SVC=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq 'to_entries[] | select(.value.Service == "it-svc-fwd-local-tpl" and .value.Port != 36001) | .value')
if [ -z "$RPROXY_SVC" ]; then echo "FAIL: rproxylocal entry not found" >&2; exit 1; fi

RPROXY_META=$(echo "$RPROXY_SVC" | jq '.Meta')
RPROXY_TEMPLATE=$(echo "$RPROXY_META" | jq -r '.template')
if [ "$RPROXY_TEMPLATE" != "HTTP_PROXY" ]; then echo "FAIL: expected template=HTTP_PROXY for rproxy, got $RPROXY_TEMPLATE" >&2; exit 1; fi

# RProxy should NOT have forwarding meta
RPROXY_FWD=$(echo "$RPROXY_META" | jq -r '.forwarding // "empty"')
if [ "$RPROXY_FWD" != "empty" ]; then echo "FAIL: rproxylocal should not have forwarding meta" >&2; exit 1; fi

echo "PASS: forwardlocal + rproxylocal separate entries"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );

    let out = run(&script);
    assert_pass(&out, "docker_forwarding_local_with_template");
}

#[test]
fn local_forwarding_local_bind_port() {
    let services_yaml = r#"
services:
  it-local-fwd-local:
    type: local
    address: 10.99.99.99
    forwardlocal:
      - port: 5000
        bind_port: 50000
"#;

    let script = format!(
        r#"{setup}

PORT=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-local-fwd-local") | .value.Port')
if [ -z "$PORT" ] || [ "$PORT" = "null" ]; then echo "FAIL: not registered with Consul" >&2; exit 1; fi
if [ "$PORT" != "50000" ]; then echo "FAIL: expected static port 50000, got $PORT" >&2; exit 1; fi

ADDR=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-local-fwd-local") | .value.Address')
if [ "$ADDR" != "10.99.99.99" ]; then echo "FAIL: expected address 10.99.99.99, got $ADDR" >&2; exit 1; fi

META=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq 'to_entries[] | select(.value.Service == "it-local-fwd-local") | .value.Meta')
FORWARDING=$(echo "$META" | jq -r '.forwarding')
if [ "$FORWARDING" != "true" ]; then echo "FAIL: missing forwarding meta" >&2; exit 1; fi

FWD_TYPE=$(echo "$META" | jq -r '.forwarding_type')
if [ "$FWD_TYPE" != "local" ]; then echo "FAIL: expected forwarding_type=local, got $FWD_TYPE" >&2; exit 1; fi

echo "PASS: local forwarding local at 10.99.99.99:50000 with forwarding_type=local"
kill %3 %2 %1 2>/dev/null || true
sleep 1
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
    );
    let out = run(&script);
    assert_pass(&out, "local_forwarding_local_bind_port");
}

#[test]
fn docker_forwarding_local_no_bind() {
    let cname = "it-fwd-local-nb";
    let services_yaml = r#"
services:
  it-svc-fwd-local-nb:
    type: docker
    match:
      project: it-svc-fwd-local-nb
    forwardlocal:
      - port: 80
"#;

    let script = format!(
        r#"{setup}
docker rm -f {cname} 2>/dev/null || true
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-fwd-local-nb" nginx:alpine
sleep 4

SVC=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq 'to_entries[] | select(.value.Service == "it-svc-fwd-local-nb") | .value')
PORT=$(echo "$SVC" | jq -r '.Port')
if [ -z "$PORT" ] || [ "$PORT" = "null" ]; then echo "FAIL: not registered with Consul" >&2; exit 1; fi
if [ "$PORT" -lt 32768 ] || [ "$PORT" -gt 61000 ]; then echo "FAIL: expected ephemeral port in 32768-61000, got $PORT" >&2; exit 1; fi

FORWARDING=$(echo "$SVC" | jq -r '.Meta.forwarding')
if [ "$FORWARDING" != "true" ]; then echo "FAIL: missing forwarding meta" >&2; exit 1; fi

FWD_TYPE=$(echo "$SVC" | jq -r '.Meta.forwarding_type')
if [ "$FWD_TYPE" != "local" ]; then echo "FAIL: expected forwarding_type=local, got $FWD_TYPE" >&2; exit 1; fi

echo "PASS: forwarding local no bind (ephemeral), port=$PORT with forwarding_type=local"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );

    let out = run(&script);
    assert_pass(&out, "docker_forwarding_local_no_bind");
}

#[test]
fn bind_ip_strict_address() {
    let cname = "it-bind-ip";
    let services_yaml = r#"
services:
  it-svc-b:
    type: docker
    match:
      project: it-svc-b
    bind_ip: 10.99.99.1
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-b.test.local"#;

    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-b" nginx:alpine
sleep 4

PORT=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-b") | .value.Port')
if [ -z "$PORT" ] || [ "$PORT" = "null" ]; then echo "FAIL: not registered with Consul" >&2; exit 1; fi

CID=$(docker inspect -f '{{{{.Id}}}}' {cname} | cut -c1-12)
MAPPING=$(lab-ops natmap --socket /tmp/natmap.sock ls | awk -v id="$CID" '$6 == id {{print $8}}')
EXPECTED="10.99.99.1:$PORT"
if [ "$MAPPING" != "$EXPECTED" ]; then echo "FAIL: expected $EXPECTED, got $MAPPING" >&2; exit 1; fi

echo "PASS: bound to $EXPECTED"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );

    let out = run(&script);
    assert_pass(&out, "Test B — bind_ip");
}

#[test]
fn bind_interface_resolved_address() {
    let cname = "it-iface";
    let services_yaml = r#"
services:
  it-svc-c:
    type: docker
    match:
      project: it-svc-c
    bind_interface: dummy0
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-c.test.local"#;

    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-c" nginx:alpine
sleep 4

PORT=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-c") | .value.Port')
if [ -z "$PORT" ] || [ "$PORT" = "null" ]; then echo "FAIL: not registered with Consul" >&2; exit 1; fi

CID=$(docker inspect -f '{{{{.Id}}}}' {cname} | cut -c1-12)
MAPPING=$(lab-ops natmap --socket /tmp/natmap.sock ls | awk -v id="$CID" '$6 == id {{print $8}}')
EXPECTED="10.99.99.1:$PORT"
if [ "$MAPPING" != "$EXPECTED" ]; then echo "FAIL: expected $EXPECTED, got $MAPPING" >&2; exit 1; fi

echo "PASS: bound to $EXPECTED"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );

    let out = run(&script);
    assert_pass(&out, "Test C — bind_interface");
}

#[test]
fn bind_interface_overrides_defaults() {
    let cname = "it-iface-override";
    let services_yaml = r#"
services:
  it-svc-override:
    type: docker
    match:
      project: it-svc-override
    bind_interface: dummy0
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-override.test.local"#;

    let defaults_yaml = r#"
  bind_ip: 1.2.3.4
"#;

    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-override" nginx:alpine
sleep 4

PORT=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-override") | .value.Port')
if [ -z "$PORT" ] || [ "$PORT" = "null" ]; then echo "FAIL: not registered with Consul" >&2; exit 1; fi

CID=$(docker inspect -f '{{{{.Id}}}}' {cname} | cut -c1-12)
MAPPING=$(lab-ops natmap --socket /tmp/natmap.sock ls | awk -v id="$CID" '$6 == id {{print $8}}')
EXPECTED="10.99.99.1:$PORT"
if [ "$MAPPING" != "$EXPECTED" ]; then echo "FAIL: expected $EXPECTED, got $MAPPING" >&2; exit 1; fi

echo "PASS: bound to $EXPECTED, ignored default 1.2.3.4"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(
            services_yaml,
            defaults_yaml,
            "",
            "--no-forwarding"
        ),
        teardown = teardown(&[cname]),
        cname = cname,
    );

    let out = run(&script);
    assert_pass(&out, "Test C — bind_interface_overrides_defaults");
}
