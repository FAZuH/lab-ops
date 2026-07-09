use super::*;

#[test]
fn container_stop_kv_delete_and_deregister() {
    let cname = "it-stop";
    let services_yaml = r#"
services:
  it-svc-i:
    type: docker
    match:
      project: it-svc-i
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-i.test.local"#;

    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-i" nginx:alpine
sleep 4

docker stop {cname}
sleep 5

PORT_AFTER=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-i") | .value.Port // empty')
if [ -n "$PORT_AFTER" ]; then
    echo "FAIL: service not deregistered from Consul" >&2
    exit 1
fi

echo "PASS: service deregistered on stop"
docker rm -f {cname} 2>/dev/null || true
kill %3 %2 %1 2>/dev/null || true
sleep 1
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        cname = cname,
    );

    let out = run(&script);
    assert_pass(&out, "Test I — deregister on stop");
}

#[test]
fn host_networked_container_skipped() {
    let cname = "it-host-net";
    let services_yaml = r#"
services:
  it-svc-hostnet:
    type: docker
    match:
      project: it-svc-hostnet
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-hostnet.test.local"#;
    let script = format!(
        r#"{setup}
docker run -d --name {cname} --network host -l "com.docker.compose.project=it-svc-hostnet" nginx:alpine
sleep 4

SVC=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-hostnet") | .value.Port // empty')
if [ -n "$SVC" ]; then echo "FAIL: host-networked container should not be registered, got port=$SVC" >&2; exit 1; fi

echo "PASS: host-networked container correctly skipped"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );
    let out = run(&script);
    assert_pass(&out, "Phase 4 — host-networked skip");
}

#[test]
fn port_not_exposed_still_matched() {
    let cname = "it-no-exp";
    let services_yaml = r#"
services:
  it-svc-noexp:
    type: docker
    match:
      project: it-svc-noexp
    rproxylocal:
    - port: 9999
      template: HTTP_PROXY
      domains:
      - it-svc-noexp.test.local"#;
    let script = format!(
        r#"{setup}
docker rm -f {cname} 2>/dev/null || true
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-noexp" nginx:alpine
sleep 4

SVC=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-noexp") | .value.Port // empty')
if [ -z "$SVC" ]; then echo "FAIL: container not registered (port exposure not required)" >&2; exit 1; fi

echo "PASS: container matched despite no port exposure"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );
    let out = run(&script);
    assert_pass(&out, "Phase 4 — no port exposure still matched");
}

#[test]
fn extra_fields_passed_to_consul_meta() {
    let cname = "it-extra";
    let services_yaml = r#"
services:
  it-svc-extra:
    type: docker
    match:
      project: it-svc-extra
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-extra.test.local
    extra:
      cluster: us-east
      max_conns: '100'"#;
    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-extra" nginx:alpine
sleep 4

META=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq 'to_entries[] | select(.value.Service == "it-svc-extra") | .value.Meta')

CLUSTER=$(echo "$META" | jq -r '.cluster // empty')
if [ "$CLUSTER" != "us-east" ]; then echo "FAIL: extra field 'cluster' not in meta, got $CLUSTER" >&2; exit 1; fi

MAX_CONNS=$(echo "$META" | jq -r '.max_conns // empty')
if [ "$MAX_CONNS" != "100" ]; then echo "FAIL: extra field 'max_conns' not in meta, got $MAX_CONNS" >&2; exit 1; fi

echo "PASS: extra fields present in Consul meta"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );
    let out = run(&script);
    assert_pass(&out, "Phase 5 — extra fields meta");
}

#[test]
fn service_id_contains_domain_slug() {
    let cname = "it-slug";
    let services_yaml = r#"
services:
  it-svc-slug:
    type: docker
    match:
      project: it-svc-slug
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it.svc.slug.test.local"#;
    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-slug" nginx:alpine
sleep 4

SVC_ID=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-slug") | .key')
EXPECTED_SLUG="it-svc-slug-test-local"

if ! echo "$SVC_ID" | grep -q "$EXPECTED_SLUG"; then
    echo "FAIL: service ID $SVC_ID does not contain expected slug $EXPECTED_SLUG" >&2
    exit 1
fi

echo "PASS: service ID contains domain slug: $SVC_ID"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );
    let out = run(&script);
    assert_pass(&out, "Phase 6 — domain slug in service ID");
}

#[test]
fn service_id_no_domain_falls_back_to_name() {
    let cname = "it-nodomain";
    let services_yaml = r#"
services:
  it-svc-nodomain:
    type: docker
    match:
      project: it-svc-nodomain
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains: []"#;
    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-nodomain" nginx:alpine
sleep 4

SVC_ID=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-nodomain") | .key')
EXPECTED_PREFIX="int-test-node-it-svc-nodomain-"

if ! echo "$SVC_ID" | grep -q "$EXPECTED_PREFIX"; then
    echo "FAIL: service ID $SVC_ID does not use name fallback (expected prefix $EXPECTED_PREFIX)" >&2
    exit 1
fi

echo "PASS: service ID uses name fallback: $SVC_ID"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );
    let out = run(&script);
    assert_pass(&out, "Phase 6 — no-domain fallback ID");
}

#[test]
fn container_id_in_consul_meta() {
    let cname = "it-cid-meta";
    let services_yaml = r#"
services:
  it-svc-cidmeta:
    type: docker
    match:
      project: it-svc-cidmeta
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-cidmeta.test.local"#;
    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-cidmeta" nginx:alpine
sleep 4

CID=$(docker inspect -f '{{{{.Id}}}}' {cname})
META_CID=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-cidmeta") | .value.Meta.container_id')

if [ "$META_CID" != "$CID" ]; then
    echo "FAIL: Meta.container_id=$META_CID does not match container ID=$CID" >&2
    exit 1
fi

echo "PASS: Meta.container_id matches container: $META_CID"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );
    let out = run(&script);
    assert_pass(&out, "Phase 6 — container_id meta");
}

#[test]
fn container_restart_reuses_port_from_state() {
    let cname = "it-reuse";
    let services_yaml = r#"
services:
  it-svc-reuse:
    type: docker
    match:
      project: it-svc-reuse
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-reuse.test.local"#;
    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-reuse" nginx:alpine
sleep 4

PORT_BEFORE=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-reuse") | .value.Port')
if [ -z "$PORT_BEFORE" ] || [ "$PORT_BEFORE" = "null" ]; then echo "FAIL: first run not registered" >&2; exit 1; fi
echo "Initial port: $PORT_BEFORE"

docker stop {cname}
sleep 4

docker rm -f {cname} 2>/dev/null || true
sleep 1

docker run -d --name {cname} -l "com.docker.compose.project=it-svc-reuse" nginx:alpine
sleep 5

PORT_AFTER=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-reuse") | .value.Port')
if [ -z "$PORT_AFTER" ] || [ "$PORT_AFTER" = "null" ]; then echo "FAIL: restarted container not registered" >&2; exit 1; fi

if [ "$PORT_BEFORE" != "$PORT_AFTER" ]; then
    echo "FAIL: port changed from $PORT_BEFORE to $PORT_AFTER (expected reuse)" >&2
    exit 1
fi

echo "PASS: port $PORT_AFTER reused across container restart"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );
    let out = run(&script);
    assert_pass(&out, "Phase 9 — port reuse on restart");
}

#[test]
fn compose_project_mismatch_skipped() {
    let cname = "it-edge-mismatch";
    let services_yaml = r#"
services:
  it-svc-mismatch:
    type: docker
    match:
      project: it-svc-mismatch
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-mismatch.test.local"#;
    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=nonexistent-project" nginx:alpine
sleep 4

SVC=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-mismatch") | .value.Port // empty')
if [ -n "$SVC" ]; then echo "FAIL: mismatched project should not register" >&2; exit 1; fi

echo "PASS: mismatched compose project correctly skipped"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );
    let out = run(&script);
    assert_pass(&out, "Phase 9 — compose project mismatch");
}

#[test]
fn container_die_event_deregistration() {
    let cname = "it-edge-die";
    let services_yaml = r#"
services:
  it-svc-die:
    type: docker
    match:
      project: it-svc-die
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-die.test.local"#;
    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-die" nginx:alpine
sleep 4

PORT=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-die") | .value.Port // empty')
if [ -z "$PORT" ]; then echo "FAIL: service not registered" >&2; exit 1; fi

docker kill {cname} >/dev/null 2>&1
sleep 5

SVC_AFTER=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-die") | .value.Port // empty')
if [ -n "$SVC_AFTER" ]; then echo "FAIL: service not deregistered after die event" >&2; exit 1; fi

echo "PASS: service deregistered on die event"
docker rm -f {cname} 2>/dev/null || true
kill %3 %2 %1 2>/dev/null || true
sleep 1
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        cname = cname,
    );
    let out = run(&script);
    assert_pass(&out, "Phase 9 — die event deregistration");
}

#[test]
fn concurrent_starts_all_registered() {
    let mut yaml_services = String::new();
    let mut cnames = Vec::new();
    for i in 0..5 {
        let project = format!("it-edge-con-{i}");
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
sleep 10

COUNT=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq '[to_entries[] | select(.value.Service | startswith("it-edge-con-"))] | length')
if [ "$COUNT" -lt 5 ]; then echo "FAIL: expected 5 services, got $COUNT" >&2; curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq keys; exit 1; fi

echo "PASS: all 5 concurrent containers registered"
docker rm -f {cnames_list} 2>/dev/null || true
kill %3 %2 %1 2>/dev/null || true
sleep 1
"#,
        setup = new_format_setup(&services_yaml, ""),
        cnames_list = cnames.join(" "),
    );
    let out = run(&script);
    assert_pass(&out, "Phase 9 — concurrent starts");
}

#[test]
fn event_loop_logs_structured_fields() {
    let cname = "it-log-fields";
    let services_yaml = r#"
services:
  it-svc-log:
    type: docker
    match:
      project: it-svc-log
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-log.test.local"#;

    let script = format!(
        r#"{setup}
# Restart daemon with debug logging so fields are visible
kill %3 2>/dev/null || true
sleep 1
NO_COLOR=1 RUST_LOG_STYLE=never RUST_LOG="info,auto_discover=debug" lab-ops auto-discover daemon /tmp/discovery.yaml \
    --state-dir /tmp/state \
    --no-forwarding \
    --consul-addr http://127.0.0.1:8500 \
    >/tmp/discovery-debug.log 2>&1 &
sleep 3

RUST_LOG=debug docker run -d --name {cname} \
    -l "com.docker.compose.project=it-svc-log" nginx:alpine
sleep 5

if ! grep -q 'container.id=' /tmp/discovery-debug.log && \
   ! grep -q 'container_id=' /tmp/discovery-debug.log; then
    echo "FAIL: no structured container.id field in logs" >&2
    head -50 /tmp/discovery-debug.log >&2
    exit 1
fi

if grep -qP 'Docker event.*:.*' /tmp/discovery-debug.log; then
    echo "FAIL: found interpolated string format in logs" >&2
    grep -P 'Docker event.*:.*' /tmp/discovery-debug.log >&2
    exit 1
fi

echo "PASS: structured log fields present"
{teardown}
"#,
        setup = new_format_setup(services_yaml, ""),
        teardown = teardown(&[cname]),
        cname = cname,
    );

    let out = run(&script);
    assert_pass(&out, "event_loop_logs_structured_fields");
}
