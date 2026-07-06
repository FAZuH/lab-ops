use super::*;

#[test]
fn invalid_yaml_config_daemon_warns_not_crash() {
    let script = r#"
set -e
CONSUL_HTTP_ADDR=http://127.0.0.1:8500

consul agent -dev -http-port=8500 >/tmp/consul.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: consul died"; cat /tmp/consul.log; exit 1; }

echo "invalid: yaml: {broken" > /tmp/bad-config.yaml

mkdir -p /tmp/state
lab-ops auto-discover sync /tmp/bad-config.yaml --state-dir /tmp/state 2>/tmp/sync-err.log && { echo "FAIL: sync should have failed"; exit 1; } || true

echo "PASS: sync correctly rejected invalid YAML"
kill %1 2>/dev/null || true
sleep 1
"#.to_string();
    let out = run(&script);
    assert_pass(&out, "Phase 3 — invalid YAML rejected");
}

#[test]
fn preprocess_script_modifies_config() {
    let cname = "it-ngx-pp";
    let services_yaml = r#"
services:
  it-svc-pp:
    type: docker
    match:
      project: it-svc-pp
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-pp.test.local
      preprocess: sed 's/test.local/preprocessed.com/g'"#;
    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-pp" nginx:alpine
sleep 5

SVC_ID=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-pp") | .key')
KV_KEY="nginx-configs/sites/${{SVC_ID}}.conf"
KV_VAL=$(curl -sf "$CONSUL_HTTP_ADDR/v1/kv/${{KV_KEY}}?raw=true")
if echo "$KV_VAL" | grep -q 'test.local'; then echo "FAIL: preprocess should have replaced test.local" >&2; exit 1; fi
if ! echo "$KV_VAL" | grep -q 'preprocessed.com'; then echo "FAIL: preprocess should produce preprocessed.com" >&2; exit 1; fi

echo "PASS: preprocess modified config"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );
    let out = run(&script);
    assert_pass(&out, "Phase 5 — preprocess");
}

#[test]
fn postprocess_script_stored_in_kv() {
    let cname = "it-ngx-post";
    let services_yaml = r#"
services:
  it-svc-post:
    type: docker
    match:
      project: it-svc-post
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-post.test.local
      postprocess: sed 's/80/8080/g'"#;
    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-post" nginx:alpine
sleep 5

SVC_ID=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-post") | .key')
POST_KEY="nginx-configs/sites/${{SVC_ID}}.postproc"
POST_VAL=$(curl -sf "$CONSUL_HTTP_ADDR/v1/kv/${{POST_KEY}}?raw=true")
if [ -z "$POST_VAL" ] || [ "$POST_VAL" = "null" ]; then
    echo "FAIL: .postproc key not found at $POST_KEY" >&2
    exit 1
fi
if ! echo "$POST_VAL" | grep -q '8080'; then
    echo "FAIL: postprocess script content incorrect" >&2
    exit 1
fi

echo "PASS: postprocess stored in KV at $POST_KEY"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );
    let out = run(&script);
    assert_pass(&out, "Phase 5 — postprocess KV");
}

#[test]
fn multi_domain_config_all_domains_in_env() {
    let cname = "it-ngx-md";
    let services_yaml = r#"
services:
  it-svc-md:
    type: docker
    match:
      project: it-svc-md
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - primary.test.local
      - alt1.test.local
      - alt2.test.local"#;
    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-md" nginx:alpine
sleep 5

SVC_ID=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-md") | .key')
KV_KEY="nginx-configs/sites/${{SVC_ID}}.conf"
KV_VAL=$(curl -sf "$CONSUL_HTTP_ADDR/v1/kv/${{KV_KEY}}?raw=true")
DOMAIN=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-md") | .value.Meta.domain')
if [ "$DOMAIN" != "primary.test.local" ]; then echo "FAIL: primary domain wrong: $DOMAIN" >&2; exit 1; fi
if ! echo "$KV_VAL" | grep -q 'primary.test.local'; then echo "FAIL: config missing primary domain" >&2; exit 1; fi

echo "PASS: primary domain=$DOMAIN, multi-domain service registered"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );
    let out = run(&script);
    assert_pass(&out, "Phase 5 — multi-domain");
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
sleep 2
if ! kill -0 $! 2>/dev/null; then echo "FAIL: natmap daemon died" >&2; cat /tmp/natmap.log; exit 1; fi

cat > /tmp/discovery.yaml <<'YAMLEOF'
node:
  name: int-test-node
defaults:
  nginx_generator: /tmp/gen-nginx
{services_yaml}
YAMLEOF

cat > /tmp/gen-nginx <<'GENEOF'
#!/bin/bash
if [ -n "${{LAB_DISCOVERY_SERVICE_NAME:-}}" ]; then
    echo "FAIL: LAB_DISCOVERY_ environment variables are set!" >&2
    exit 1
fi
if [ -z "${{AUTO_DISCOVER_SERVICE_NAME:-}}" ]; then
    echo "FAIL: AUTO_DISCOVER_ environment variables are NOT set!" >&2
    exit 1
fi
cat <<EOF
# Service: ${{AUTO_DISCOVER_SERVICE_NAME:-unknown}}
server {{
    server_name ${{AUTO_DISCOVER_DOMAIN:-_}};
    listen ${{AUTO_DISCOVER_PROXY_IP:-127.0.0.1}}:80;
    proxy_pass http://${{AUTO_DISCOVER_BIND_IP}}:${{AUTO_DISCOVER_HOST_PORT}}/;
}}
EOF
GENEOF
chmod +x /tmp/gen-nginx

docker run -d --name {cname} -l "com.docker.compose.project=it-svc-restart" nginx:alpine
sleep 4

SVC_BEFORE=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-restart") | .value.Port // empty')
if [ -n "$SVC_BEFORE" ]; then echo "FAIL: service registered before daemon start" >&2; exit 1; fi

lab-ops auto-discover daemon /tmp/discovery.yaml \
    --state-dir /tmp/state \
    --no-forwarding --no-nginx \
    --consul-addr http://127.0.0.1:8500 \
    >/tmp/discovery.log 2>&1 &
sleep 5

SVC_AFTER=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-restart") | .value.Port // empty')
if [ -z "$SVC_AFTER" ] || [ "$SVC_AFTER" = "null" ]; then
    echo "FAIL: service not registered after daemon start" >&2
    cat /tmp/discovery.log
    exit 1
fi

echo "PASS: daemon start picked up existing container"
docker rm -f {cname} 2>/dev/null || true
kill %1 %2 %3 2>/dev/null || true
sleep 1
"#,
    );
    let out = run(&script);
    assert_pass(&out, "Phase 2 — restart auto-discover");
}

#[test]
fn restart_natmap_new_container_registered_after_recovery() {
    let cname = "it-restart-nm";
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
defaults:
  nginx_generator: /tmp/gen-nginx
{services_yaml}
YAMLEOF

cat > /tmp/gen-nginx <<'GENEOF'
#!/bin/bash
if [ -n "${{LAB_DISCOVERY_SERVICE_NAME:-}}" ]; then
    echo "FAIL: LAB_DISCOVERY_ environment variables are set!" >&2
    exit 1
fi
if [ -z "${{AUTO_DISCOVER_SERVICE_NAME:-}}" ]; then
    echo "FAIL: AUTO_DISCOVER_ environment variables are NOT set!" >&2
    exit 1
fi
cat <<EOF
# Service: ${{AUTO_DISCOVER_SERVICE_NAME:-unknown}}
server {{
    server_name ${{AUTO_DISCOVER_DOMAIN:-_}};
    listen ${{AUTO_DISCOVER_PROXY_IP:-127.0.0.1}}:80;
    proxy_pass http://${{AUTO_DISCOVER_BIND_IP}}:${{AUTO_DISCOVER_HOST_PORT}}/;
}}
EOF
GENEOF
chmod +x /tmp/gen-nginx

lab-ops auto-discover daemon /tmp/discovery.yaml \
    --state-dir /tmp/state \
    --no-forwarding --no-nginx \
    --consul-addr http://127.0.0.1:8500 \
    >/tmp/discovery.log 2>&1 &
sleep 2
if ! kill -0 $! 2>/dev/null; then echo "FAIL: auto-discover daemon died" >&2; cat /tmp/discovery.log; exit 1; fi

docker run -d --name it-nmrestart-a -l "com.docker.compose.project=it-svc-nmrestart" nginx:alpine
sleep 4
A=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-nmrestart") | .value.Port // empty')
if [ -z "$A" ] || [ "$A" = "null" ]; then echo "FAIL: first container not registered" >&2; exit 1; fi
echo "First container OK: port=$A"

kill $NATMAP_PID 2>/dev/null || true
sleep 2

docker run -d --name it-nmrestart-b -l "com.docker.compose.project=it-svc-nmrestart" nginx:alpine
sleep 4
B=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq '[to_entries[] | select(.value.Service == "it-svc-nmrestart")] | length')
if [ "$B" -gt 1 ]; then echo "FAIL: second container registered while natmap was down" >&2; exit 1; fi
echo "Second container correctly not registered (natmap was down)"

rm -f /tmp/natmap_state.json
lab-ops natmap daemon --socket /tmp/natmap.sock --state /tmp/natmap_state.json --socket-group root >/tmp/natmap2.log 2>&1 &
sleep 3

docker rm -f it-nmrestart-b 2>/dev/null || true
sleep 1

docker run -d --name {cname} -l "com.docker.compose.project=it-svc-nmrestart" nginx:alpine
sleep 5
C=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-nmrestart") | .value.Port // empty' | wc -l)
if [ "$C" -lt 1 ]; then echo "FAIL: no services registered after natmap recovery" >&2; exit 1; fi

echo "PASS: new container registered after natmap recovery"
docker rm -f {cname} it-nmrestart-a 2>/dev/null || true
kill %1 %2 %3 %4 2>/dev/null || true
sleep 1
"#,
    );
    let out = run(&script);
    assert_pass(&out, "Phase 2 — restart natmap");
}

#[test]
fn add_service_to_config_picked_up_on_sync() {
    let script = r#"
set -e
NATMAP_SOCKET=/tmp/natmap.sock
CONSUL_HTTP_ADDR=http://127.0.0.1:8500

consul agent -dev -http-port=8500 >/tmp/consul.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: consul died"; cat /tmp/consul.log; exit 1; }

rm -f /tmp/natmap_state.json
lab-ops natmap daemon --socket $NATMAP_SOCKET --state /tmp/natmap_state.json --socket-group root >/tmp/natmap.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: natmap died"; cat /tmp/natmap.log; exit 1; }

cat > /tmp/gen-nginx <<'GENEOF'
#!/bin/bash
echo "server { server_name ${AUTO_DISCOVER_DOMAIN:-_}; listen 80; }"
GENEOF
chmod +x /tmp/gen-nginx

cat > /tmp/discovery.yaml <<'YAMLEOF'
node:
  name: int-test-node
defaults:
  nginx_generator: /tmp/gen-nginx
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
YAMLEOF

lab-ops auto-discover daemon /tmp/discovery.yaml --state-dir /tmp/state --no-forwarding --no-nginx --consul-addr $CONSUL_HTTP_ADDR >/tmp/discovery.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: daemon died"; cat /tmp/discovery.log; exit 1; }

docker run -d --name it-cfg-a -l "com.docker.compose.project=it-svc-cfg-a" nginx:alpine
sleep 4
A=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-cfg-a") | .value.Port // empty')
if [ -z "$A" ]; then echo "FAIL: service A not registered" >&2; exit 1; fi

kill %3 2>/dev/null || true
lab-ops natmap --socket $NATMAP_SOCKET clear >/dev/null 2>&1 || true
sleep 1

cat > /tmp/discovery.yaml <<'YAMLEOF'
node:
  name: int-test-node
defaults:
  nginx_generator: /tmp/gen-nginx
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
      - it-svc-cfg-b.test.local
YAMLEOF

lab-ops auto-discover sync /tmp/discovery.yaml --state-dir /tmp/state >/tmp/sync.log 2>&1

docker run -d --name it-cfg-b -l "com.docker.compose.project=it-svc-cfg-b" nginx:alpine
sleep 5

COUNT=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq '[to_entries[] | select(.value.Service == "it-svc-cfg-a" or .value.Service == "it-svc-cfg-b")] | length')
if [ "$COUNT" -lt 2 ]; then echo "FAIL: expected 2 services, got $COUNT" >&2; cat /tmp/sync.log; exit 1; fi

echo "PASS: new service registered after config change"
docker rm -f it-cfg-a it-cfg-b 2>/dev/null || true
kill %1 %2 %3 %4 2>/dev/null || true
sleep 1
"#.to_string();
    let out = run(&script);
    assert_pass(&out, "Phase 3 — add service to config");
}

#[test]
fn remove_service_from_config_stale_deregistered() {
    let script = r#"
set -e
NATMAP_SOCKET=/tmp/natmap.sock
CONSUL_HTTP_ADDR=http://127.0.0.1:8500

consul agent -dev -http-port=8500 >/tmp/consul.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: consul died"; cat /tmp/consul.log; exit 1; }

rm -f /tmp/natmap_state.json
lab-ops natmap daemon --socket $NATMAP_SOCKET --state /tmp/natmap_state.json --socket-group root >/tmp/natmap.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: natmap died"; cat /tmp/natmap.log; exit 1; }

cat > /tmp/gen-nginx <<'GENEOF'
#!/bin/bash
echo "server { server_name ${AUTO_DISCOVER_DOMAIN:-_}; listen 80; }"
GENEOF
chmod +x /tmp/gen-nginx

cat > /tmp/discovery.yaml <<'YAMLEOF'
node:
  name: int-test-node
defaults:
  nginx_generator: /tmp/gen-nginx
services:
  it-svc-cfg-rm:
    type: docker
    match:
      project: it-svc-cfg-rm
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-cfg-rm.test.local
YAMLEOF

lab-ops auto-discover daemon /tmp/discovery.yaml --state-dir /tmp/state --no-forwarding --no-nginx --consul-addr $CONSUL_HTTP_ADDR >/tmp/discovery.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: daemon died"; cat /tmp/discovery.log; exit 1; }

docker run -d --name it-cfg-rm -l "com.docker.compose.project=it-svc-cfg-rm" nginx:alpine
sleep 4
A=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-cfg-rm") | .value.Port // empty')
if [ -z "$A" ]; then echo "FAIL: service not registered initially" >&2; exit 1; fi

kill %3 2>/dev/null || true
lab-ops natmap --socket $NATMAP_SOCKET clear >/dev/null 2>&1 || true
docker rm -f it-cfg-rm 2>/dev/null || true
sleep 2

cat > /tmp/discovery.yaml <<'YAMLEOF'
services: {}
YAMLEOF

lab-ops auto-discover sync /tmp/discovery.yaml --state-dir /tmp/state >/tmp/sync.log 2>&1 || true

REMAINING=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Meta.server_name == "int-test-node") | .key // empty')
if [ -n "$REMAINING" ]; then echo "FAIL: stale service still registered: $REMAINING" >&2; exit 1; fi

echo "PASS: service deregistered after empty config"
kill %1 %2 %3 2>/dev/null || true
sleep 1
"#.to_string();
    let out = run(&script);
    assert_pass(&out, "Phase 3 — remove service from config");
}

#[test]
fn change_bind_ip_service_reregisters() {
    let cname = "it-cfg-ip";
    let script = format!(
        r#"
set -e
NATMAP_SOCKET=/tmp/natmap.sock
CONSUL_HTTP_ADDR=http://127.0.0.1:8500

consul agent -dev -http-port=8500 >/tmp/consul.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || {{ echo "FAIL: consul died"; cat /tmp/consul.log; exit 1; }}

ip link add dummy0 type dummy 2>/dev/null || true
ip addr add 10.99.99.1/24 dev dummy0 2>/dev/null || true
ip link set dummy0 up

rm -f /tmp/natmap_state.json
lab-ops natmap daemon --socket $NATMAP_SOCKET --state /tmp/natmap_state.json --socket-group root >/tmp/natmap.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || {{ echo "FAIL: natmap died"; cat /tmp/natmap.log; exit 1; }}

cat > /tmp/gen-nginx <<'GENEOF'
#!/bin/bash
echo "server {{ server_name ${{AUTO_DISCOVER_DOMAIN:-_}}; listen 80; }}"
GENEOF
chmod +x /tmp/gen-nginx

cat > /tmp/discovery.yaml <<'YAMLEOF'
node:
  name: int-test-node
defaults:
  nginx_generator: /tmp/gen-nginx
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
      - it-cfg-ip.test.local
YAMLEOF

lab-ops auto-discover daemon /tmp/discovery.yaml --state-dir /tmp/state --no-forwarding --no-nginx --consul-addr $CONSUL_HTTP_ADDR >/tmp/discovery.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || {{ echo "FAIL: daemon died"; cat /tmp/discovery.log; exit 1; }}

docker run -d --name {cname} -l "com.docker.compose.project=it-cfg-ip-svc" nginx:alpine
sleep 4
ADDR1=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-cfg-ip-svc") | .value.Address')
if [ "$ADDR1" != "127.0.0.1" ]; then echo "FAIL: expected Address=127.0.0.1, got $ADDR1" >&2; exit 1; fi

kill %3 2>/dev/null || true
lab-ops natmap --socket $NATMAP_SOCKET clear >/dev/null 2>&1 || true
sleep 1

cat > /tmp/discovery.yaml <<'YAMLEOF'
node:
  name: int-test-node
defaults:
  nginx_generator: /tmp/gen-nginx
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
      - it-cfg-ip.test.local
YAMLEOF

lab-ops auto-discover sync /tmp/discovery.yaml --state-dir /tmp/state >/tmp/sync.log 2>&1

ADDR2=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-cfg-ip-svc") | .value.Address')
if [ "$ADDR2" != "10.99.99.1" ]; then echo "FAIL: expected Address=10.99.99.1 after change, got $ADDR2" >&2; exit 1; fi

echo "PASS: bind_ip updated from 127.0.0.1 to $ADDR2"
docker rm -f {cname} 2>/dev/null || true
kill %1 %2 %3 %4 2>/dev/null || true
sleep 1
"#,
    );
    let out = run(&script);
    assert_pass(&out, "Phase 3 — change bind_ip");
}

#[test]
fn remove_all_services_clean_slate() {
    let cname = "it-cfg-all";
    let script = format!(
        r#"
set -e
NATMAP_SOCKET=/tmp/natmap.sock
CONSUL_HTTP_ADDR=http://127.0.0.1:8500

consul agent -dev -http-port=8500 >/tmp/consul.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || {{ echo "FAIL: consul died"; cat /tmp/consul.log; exit 1; }}

rm -f /tmp/natmap_state.json
lab-ops natmap daemon --socket $NATMAP_SOCKET --state /tmp/natmap_state.json --socket-group root >/tmp/natmap.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || {{ echo "FAIL: natmap died"; cat /tmp/natmap.log; exit 1; }}

cat > /tmp/gen-nginx <<'GENEOF'
#!/bin/bash
echo "server {{ server_name ${{AUTO_DISCOVER_DOMAIN:-_}}; listen 80; }}"
GENEOF
chmod +x /tmp/gen-nginx

cat > /tmp/discovery.yaml <<'YAMLEOF'
node:
  name: int-test-node
defaults:
  nginx_generator: /tmp/gen-nginx
services:
  it-cfg-all-svc:
    type: docker
    match:
      project: it-cfg-all-svc
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-cfg-all.test.local
YAMLEOF

lab-ops auto-discover daemon /tmp/discovery.yaml --state-dir /tmp/state --no-forwarding --no-nginx --consul-addr $CONSUL_HTTP_ADDR >/tmp/discovery.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || {{ echo "FAIL: daemon died"; cat /tmp/discovery.log; exit 1; }}

docker run -d --name {cname} -l "com.docker.compose.project=it-cfg-all-svc" nginx:alpine
sleep 4
A=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-cfg-all-svc") | .value.Port // empty')
if [ -z "$A" ]; then echo "FAIL: service not registered" >&2; exit 1; fi

kill %3 2>/dev/null || true
lab-ops natmap --socket $NATMAP_SOCKET clear >/dev/null 2>&1 || true
docker rm -f {cname} 2>/dev/null || true
sleep 2

cat > /tmp/discovery.yaml <<'YAMLEOF'
services: {}
YAMLEOF

lab-ops auto-discover sync /tmp/discovery.yaml --state-dir /tmp/state >/tmp/sync.log 2>&1 || true

REMAINING=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Meta.server_name == "int-test-node") | .key // empty')
if [ -n "$REMAINING" ]; then echo "FAIL: stale registrations remain: $REMAINING" >&2; exit 1; fi

echo "PASS: all services deregistered"
kill %1 %2 %3 2>/dev/null || true
sleep 1
"#,
    );
    let out = run(&script);
    assert_pass(&out, "Phase 3 — remove all services");
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
    let _script = format!(
        r#"{setup}
for cn in {cnames_list}; do
    docker run -d --name "$cn" -l "com.docker.compose.project=$cn" nginx:alpine
done
sleep 8

COUNT=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq '[to_entries[] | select(.value.Service | startswith("it-large-"))] | length')
if [ "$COUNT" -lt 5 ]; then echo "FAIL: expected 5 services, got $COUNT" >&2; exit 1; fi

echo "PASS: all 5 services registered"
docker rm -f {cnames_list} 2>/dev/null || true
kill %3 %2 %1 2>/dev/null || true
sleep 1
"#,
        setup = new_format_setup(&services_yaml, ""),
        cnames_list = cnames.join(" "),
    );
}
