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
{registered}

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
        registered = wait_for_consul_service("it-svc-fwd-local", 30),
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
{registered}

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
        registered = wait_for_service_count("it-svc-fwd-local-tpl", 2, 30),
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
{wait}
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
        wait = wait_for_consul_service("it-local-fwd-local", 15),
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
{registered}

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
        registered = wait_for_consul_service("it-svc-fwd-local-nb", 30),
        teardown = teardown(&[cname]),
        cname = cname,
    );

    let out = run(&script);
    assert_pass(&out, "docker_forwarding_local_no_bind");
}

/// Where the daemon bound a mapping, read from the natmap API rather than the
/// `ls` table, so a column reorder or a re-pad cannot move the field out from
/// under the check. `$cid` must be set.
fn natmap_host_addr() -> String {
    r#"MAPPING=$(curl -sf --unix-socket /tmp/natmap.sock http://localhost/mappings \
  | jq -r --arg id "$CID" '.docker[] | select(.container_id | startswith($id)) | .request.host_addr')"#
        .to_string()
}

/// One scenario per bind source. `bind_ip` is taken verbatim; `bind_interface`
/// is resolved to the interface's address; `bind_interface` also beats a
/// `defaults:` block. All three must land on dummy0's 10.99.99.1.
const BIND_SOURCES: [(&str, &str, &str, &str); 3] = [
    ("it-bind-ip", "it-svc-b", "bind_ip: 10.99.99.1", ""),
    ("it-iface", "it-svc-c", "bind_interface: dummy0", ""),
    (
        "it-iface-override",
        "it-svc-override",
        "bind_interface: dummy0",
        "\n  bind_ip: 1.2.3.4\n",
    ),
];

#[test]
fn docker_bind_address_from_config() {
    for (cname, svc, bind_line, defaults_yaml) in BIND_SOURCES {
        let services_yaml = format!(
            r#"
services:
  {svc}:
    type: docker
    match:
      project: {svc}
    {bind_line}
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - {svc}.test.local"#
        );

        let script = format!(
            r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project={svc}" nginx:alpine
{registered}

PORT=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "{svc}") | .value.Port')
if [ -z "$PORT" ] || [ "$PORT" = "null" ]; then echo "FAIL: {svc} not registered with Consul" >&2; exit 1; fi

CID=$(docker inspect -f '{{{{.Id}}}}' {cname} | cut -c1-12)
{mapping}
EXPECTED="10.99.99.1:$PORT"
if [ "$MAPPING" != "$EXPECTED" ]; then echo "FAIL: {svc}: expected $EXPECTED, got $MAPPING" >&2; exit 1; fi

echo "PASS: {svc} bound to $EXPECTED"
{teardown}
"#,
            setup = new_format_setup_with_defaults_ext(
                &services_yaml,
                defaults_yaml,
                "",
                "--no-forwarding"
            ),
            registered = wait_for_consul_service(svc, 30),
            mapping = natmap_host_addr(),
            teardown = teardown(&[cname]),
            cname = cname,
            svc = svc,
        );

        let out = run(&script);
        assert_pass(&out, &format!("bind address from {svc}"));
    }
}
