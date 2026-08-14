use super::*;

#[test]
fn preserve_src_ip_global_default_creates_policy_route() {
    let cname = "it-preserve-def";
    let services_yaml = r#"
services:
  it-svc-preserve:
    type: docker
    match:
      project: it-svc-preserve
    forwardremote:
    - port: 80
      ext_ip: 203.0.113.43
      ext_ports:
      - 36000
      proto: tcp"#;
    let defaults_yaml = r#"
  preserve_src_ip: true
  preserve_src_ip_gateway: "10.99.99.1"
"#;
    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-preserve" nginx:alpine
sleep 4

# Should add ip rule and route to table 100
IP_RULE=$(ip rule show)
if ! echo "$IP_RULE" | grep -q "lookup 100"; then
    echo "FAIL: ip rule not found for lookup 100" >&2
    echo "$IP_RULE"
    exit 1
fi

IP_ROUTE=$(ip route show table 100)
if ! echo "$IP_ROUTE" | grep -q "default via 10.99.99.1"; then
    echo "FAIL: ip route not found for default via 10.99.99.1" >&2
    echo "$IP_ROUTE"
    exit 1
fi

# Local-subnet routes (e.g. dummy0) must also be cloned into table 100
# so traffic to local networks/containers uses the correct interface
# instead of the proxy gateway.
if ! echo "$IP_ROUTE" | grep -q "10.99.99.0/24"; then
    echo "FAIL: local route 10.99.99.0/24 not cloned into table 100" >&2
    echo "$IP_ROUTE"
    exit 1
fi

echo "PASS: global preserve_src_ip created policy route with cloned local routes"
{teardown}
"#,
        setup =
            new_format_setup_with_defaults_ext(services_yaml, defaults_yaml, "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );

    let out = run(&script);
    assert_pass(&out, "Test J — preserve_src_ip global default");
}

#[test]
fn preserve_src_ip_per_service_overrides_default_false() {
    let cname = "it-preserve-svc";
    let services_yaml = r#"
services:
  it-svc-preserve-svc:
    type: docker
    match:
      project: it-svc-preserve-svc
    forwardremote:
    - port: 80
      ext_ip: 203.0.113.43
      ext_ports:
      - 36000
      proto: tcp
      preserve_src_ip: true
      preserve_src_ip_gateway: "10.99.99.1"
"#;
    let defaults_yaml = r#"
  preserve_src_ip: false
"#;
    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-preserve-svc" nginx:alpine
sleep 4

IP_RULE=$(ip rule show)
if ! echo "$IP_RULE" | grep -q "lookup 100"; then
    echo "FAIL: ip rule not found for lookup 100" >&2
    exit 1
fi

echo "PASS: per-service preserve_src_ip overrides default"
{teardown}
"#,
        setup =
            new_format_setup_with_defaults_ext(services_yaml, defaults_yaml, "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );
    let out = run(&script);
    assert_pass(&out, "Test J — preserve_src_ip per-service override");
}

#[test]
fn preserve_src_ip_false_no_policy_route() {
    let cname = "it-preserve-false";
    let services_yaml = r#"
services:
  it-svc-preserve-false:
    type: docker
    match:
      project: it-svc-preserve-false
    forwardremote:
    - port: 80
      ext_ip: 203.0.113.43
      ext_ports:
      - 36000
      proto: tcp"#;
    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-preserve-false" nginx:alpine
sleep 4

IP_RULE=$(ip rule show)
if echo "$IP_RULE" | grep -q "lookup 100"; then
    echo "FAIL: ip rule found for lookup 100, but preserve_src_ip is false" >&2
    exit 1
fi

echo "PASS: preserve_src_ip false skips policy route"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );
    let out = run(&script);
    assert_pass(&out, "Test J — preserve_src_ip false");
}

#[test]
fn preserve_src_ip_consul_meta_propagated() {
    let cname = "it-preserve-meta";
    let services_yaml = r#"
services:
  it-svc-preserve-meta:
    type: docker
    match:
      project: it-svc-preserve-meta
    forwardremote:
    - port: 80
      ext_ip: 203.0.113.43
      ext_ports:
      - 36000
      proto: tcp
      preserve_src_ip: true
      preserve_src_ip_gateway: "10.99.99.1"
"#;
    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-preserve-meta" nginx:alpine
sleep 4

SVC=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq 'to_entries[] | select(.value.Service == "it-svc-preserve-meta") | .value')
PRESERVE=$(echo "$SVC" | jq -r '.Meta.preserve_src_ip')
if [ "$PRESERVE" != "true" ]; then echo "FAIL: missing preserve_src_ip meta: $PRESERVE" >&2; exit 1; fi

echo "PASS: preserve_src_ip meta propagated to consul"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );
    let out = run(&script);
    assert_pass(&out, "Test J — preserve_src_ip consul meta");
}

#[test]
fn policy_route_idempotent() {
    let cname = "it-preserve-idemp";
    let services_yaml = r#"
services:
  it-svc-preserve-idemp:
    type: docker
    match:
      project: it-svc-preserve-idemp
    forwardremote:
    - port: 80
      ext_ip: 203.0.113.43
      ext_ports:
      - 36000
      proto: tcp
      preserve_src_ip: true
      preserve_src_ip_gateway: "10.99.99.1"
"#;
    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-preserve-idemp" nginx:alpine
sleep 4

# Run sync manually again
lab-ops auto-discover sync $CONSUL_HTTP_ADDR >/tmp/sync.log 2>&1 || true

COUNT=$(ip rule show | grep -c "lookup 100" || true)
if [ "$COUNT" -ne 1 ]; then echo "FAIL: expected 1 ip rule for lookup 100, got $COUNT" >&2; exit 1; fi

echo "PASS: policy route is idempotent"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );
    let out = run(&script);
    assert_pass(&out, "Test J — preserve_src_ip idempotent");
}

#[test]
fn container_stop_removes_policy_route() {
    let cname = "it-preserve-stop";
    let services_yaml = r#"
services:
  it-svc-preserve-stop:
    type: docker
    match:
      project: it-svc-preserve-stop
    forwardremote:
    - port: 80
      ext_ip: 203.0.113.43
      ext_ports:
      - 36000
      proto: tcp
      preserve_src_ip: true
      preserve_src_ip_gateway: "10.99.99.1"
"#;
    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-preserve-stop" nginx:alpine
sleep 4

IP_RULE=$(ip rule show)
if ! echo "$IP_RULE" | grep -q "lookup 100"; then
    echo "FAIL: ip rule not found for lookup 100" >&2
    exit 1
fi

docker stop {cname}
sleep 4

IP_RULE_AFTER=$(ip rule show)
if echo "$IP_RULE_AFTER" | grep -q "lookup 100"; then
    echo "FAIL: ip rule for lookup 100 still exists after container stop" >&2
    exit 1
fi

echo "PASS: policy route removed on container stop"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );
    let out = run(&script);
    assert_pass(&out, "Test J — preserve_src_ip stop removes route");
}
