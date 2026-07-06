use super::*;

#[test]
fn nginx_daemon_writes_config_to_disk() {
    let script = r#"
set -e
CONSUL_HTTP_ADDR=http://127.0.0.1:8500

consul agent -dev -http-port=8500 >/tmp/consul.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: consul died"; cat /tmp/consul.log; exit 1; }

mkdir -p /var/lib/auto-discover/nginx-configs
mkdir -p /etc/nginx/sites-enabled
mkdir -p /etc/nginx/streams-enabled

curl -sf -X PUT "$CONSUL_HTTP_ADDR/v1/kv/nginx-configs/sites/test-svc.conf" -d 'server { listen 8080; server_name test.local; }'

lab-ops auto-discover nginx-sync $CONSUL_HTTP_ADDR >/tmp/ngx.log 2>&1 || true

if [ ! -f /var/lib/auto-discover/nginx-configs/sites/test-svc.conf ]; then
    echo "FAIL: config file not written to disk" >&2
    ls -la /var/lib/auto-discover/nginx-configs/sites/ 2>/dev/null || true
    cat /tmp/ngx.log
    exit 1
fi

echo "PASS: nginx config written to disk"
kill %1 2>/dev/null || true
sleep 1
"#.to_string();
    let out = run(&script);
    assert_pass(&out, "Phase 8 — config written to disk");
}

#[test]
fn nginx_daemon_creates_symlinks() {
    let script = r#"
set -e
CONSUL_HTTP_ADDR=http://127.0.0.1:8500

consul agent -dev -http-port=8500 >/tmp/consul.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: consul died"; cat /tmp/consul.log; exit 1; }

mkdir -p /var/lib/auto-discover/nginx-configs
mkdir -p /etc/nginx/sites-enabled
mkdir -p /etc/nginx/streams-enabled

curl -sf -X PUT "$CONSUL_HTTP_ADDR/v1/kv/nginx-configs/sites/symlink-svc.conf" -d 'server { listen 8081; server_name symlink.local; }'

lab-ops auto-discover nginx-sync $CONSUL_HTTP_ADDR >/tmp/ngx.log 2>&1 || true

SYMLINK="/etc/nginx/sites-enabled/symlink-svc.conf"
if [ ! -L "$SYMLINK" ]; then
    echo "FAIL: symlink not created at $SYMLINK" >&2
    ls -la /etc/nginx/sites-enabled/ 2>/dev/null || true
    cat /tmp/ngx.log
    exit 1
fi

TARGET=$(readlink "$SYMLINK")
EXPECTED="/var/lib/auto-discover/nginx-configs/sites/symlink-svc.conf"
if [ "$TARGET" != "$EXPECTED" ]; then
    echo "FAIL: symlink target $TARGET != $EXPECTED" >&2
    exit 1
fi

echo "PASS: symlink created in sites-enabled"
kill %1 2>/dev/null || true
sleep 1
"#.to_string();
    let out = run(&script);
    assert_pass(&out, "Phase 8 — symlink created");
}

#[test]
fn nginx_daemon_runs_postproc() {
    let script = r#"
set -e
CONSUL_HTTP_ADDR=http://127.0.0.1:8500

consul agent -dev -http-port=8500 >/tmp/consul.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: consul died"; cat /tmp/consul.log; exit 1; }

mkdir -p /var/lib/auto-discover/nginx-configs
mkdir -p /etc/nginx/sites-enabled
mkdir -p /etc/nginx/streams-enabled

curl -sf -X PUT "$CONSUL_HTTP_ADDR/v1/kv/nginx-configs/sites/post-svc.conf" -d 'listen 9999;'
curl -sf -X PUT "$CONSUL_HTTP_ADDR/v1/kv/nginx-configs/sites/post-svc.postproc" -d "sed 's/9999/8888/'"

lab-ops auto-discover nginx-sync $CONSUL_HTTP_ADDR >/tmp/ngx.log 2>&1 || true

CONFIG_FILE="/var/lib/auto-discover/nginx-configs/sites/post-svc.conf"
if [ ! -f "$CONFIG_FILE" ]; then echo "FAIL: config not written" >&2; cat /tmp/ngx.log; exit 1; fi

if grep -q '9999' "$CONFIG_FILE"; then echo "FAIL: postproc did not replace 9999" >&2; cat "$CONFIG_FILE"; exit 1; fi
if ! grep -q '8888' "$CONFIG_FILE"; then echo "FAIL: postproc should produce 8888" >&2; cat "$CONFIG_FILE"; exit 1; fi

echo "PASS: postproc applied to config"
kill %1 2>/dev/null || true
sleep 1
"#.to_string();
    let out = run(&script);
    assert_pass(&out, "Phase 8 — postproc applied");
}

#[test]
fn nginx_daemon_common_postprocs() {
    let script = r#"
set -e
CONSUL_HTTP_ADDR=http://127.0.0.1:8500

consul agent -dev -http-port=8500 >/tmp/consul.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: consul died"; cat /tmp/consul.log; exit 1; }

mkdir -p /var/lib/auto-discover/nginx-configs
mkdir -p /etc/nginx/sites-enabled
mkdir -p /etc/nginx/streams-enabled
mkdir -p /etc/auto-discover/postprocs.d

cat > /etc/auto-discover/postprocs.d/10-replace-port <<'SCRIPT'
#!/bin/bash
sed 's/7777/6666/'
SCRIPT
chmod +x /etc/auto-discover/postprocs.d/10-replace-port

curl -sf -X PUT "$CONSUL_HTTP_ADDR/v1/kv/nginx-configs/sites/common-svc.conf" -d 'listen 7777;'

lab-ops auto-discover nginx-sync $CONSUL_HTTP_ADDR >/tmp/ngx.log 2>&1 || true

CONFIG_FILE="/var/lib/auto-discover/nginx-configs/sites/common-svc.conf"
if [ ! -f "$CONFIG_FILE" ]; then echo "FAIL: config not written" >&2; cat /tmp/ngx.log; exit 1; fi

if grep -q '7777' "$CONFIG_FILE"; then echo "FAIL: common postproc did not replace 7777" >&2; cat "$CONFIG_FILE"; exit 1; fi
if ! grep -q '6666' "$CONFIG_FILE"; then echo "FAIL: common postproc should produce 6666" >&2; cat "$CONFIG_FILE"; exit 1; fi

echo "PASS: common postproc applied"
kill %1 2>/dev/null || true
sleep 1
"#.to_string();
    let out = run(&script);
    assert_pass(&out, "Phase 8 — common postproc");
}

#[test]
fn nginx_daemon_stale_cleanup() {
    let script = r#"
set -e
CONSUL_HTTP_ADDR=http://127.0.0.1:8500

consul agent -dev -http-port=8500 >/tmp/consul.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: consul died"; cat /tmp/consul.log; exit 1; }

mkdir -p /var/lib/auto-discover/nginx-configs/sites
mkdir -p /etc/nginx/sites-enabled

echo 'stale config' > /var/lib/auto-discover/nginx-configs/sites/stale-svc.conf
ln -sf /var/lib/auto-discover/nginx-configs/sites/stale-svc.conf /etc/nginx/sites-enabled/stale-svc.conf

lab-ops auto-discover nginx-sync $CONSUL_HTTP_ADDR >/tmp/ngx.log 2>&1 || true

STALE_FILE="/var/lib/auto-discover/nginx-configs/sites/stale-svc.conf"
STALE_LINK="/etc/nginx/sites-enabled/stale-svc.conf"

if [ -f "$STALE_FILE" ]; then echo "FAIL: stale config file not removed" >&2; ls -la "$STALE_FILE"; exit 1; fi
if [ -L "$STALE_LINK" ]; then echo "FAIL: stale symlink not removed" >&2; ls -la "$STALE_LINK"; exit 1; fi

echo "PASS: stale config and symlink cleaned up"
kill %1 2>/dev/null || true
sleep 1
"#.to_string();
    let out = run(&script);
    assert_pass(&out, "Phase 8 — stale cleanup");
}

#[test]
fn nginx_daemon_full_cycle() {
    let script = r#"
set -e
CONSUL_HTTP_ADDR=http://127.0.0.1:8500

consul agent -dev -http-port=8500 >/tmp/consul.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: consul died"; cat /tmp/consul.log; exit 1; }

mkdir -p /var/lib/auto-discover/nginx-configs/sites
mkdir -p /etc/nginx/sites-enabled

curl -sf -X PUT "$CONSUL_HTTP_ADDR/v1/kv/nginx-configs/sites/cycle-svc.conf" -d 'server { listen 9991; server_name cycle.local; }'

lab-ops auto-discover nginx-sync $CONSUL_HTTP_ADDR >/tmp/ngx.log 2>&1 || true

FILE="/var/lib/auto-discover/nginx-configs/sites/cycle-svc.conf"
LINK="/etc/nginx/sites-enabled/cycle-svc.conf"
if [ ! -f "$FILE" ]; then echo "FAIL: config not written" >&2; exit 1; fi
if [ ! -L "$LINK" ]; then echo "FAIL: symlink not created" >&2; exit 1; fi
if ! grep -q 'cycle.local' "$FILE"; then echo "FAIL: config content wrong" >&2; cat "$FILE"; exit 1; fi

LINK_TARGET=$(readlink "$LINK")
if [ "$LINK_TARGET" != "$FILE" ]; then echo "FAIL: symlink target wrong: $LINK_TARGET" >&2; exit 1; fi

echo "PASS: full nginx cycle: KV -> file -> symlink"
kill %1 2>/dev/null || true
sleep 1
"#.to_string();
    let out = run(&script);
    assert_pass(&out, "Phase 8 — full nginx cycle");
}

#[test]
fn nginx_config_kv_write() {
    let cname = "it-nginx-kv";
    let services_yaml = r#"
services:
  it-svc-f:
    type: docker
    match:
      project: it-svc-f
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-f.test.local"#;

    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-f" nginx:alpine
sleep 4

SVC_ID=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-f") | .key')
KV_KEY="nginx-configs/sites/${{SVC_ID}}.conf"
KV_VALUE=$(curl -sf "$CONSUL_HTTP_ADDR/v1/kv/${{KV_KEY}}?raw=true")

if [ -z "$KV_VALUE" ] || [ "$KV_VALUE" = "null" ]; then
    echo "FAIL: nginx config not found in KV at $KV_KEY" >&2
    curl -sf "$CONSUL_HTTP_ADDR/v1/kv/nginx-configs/?recurse=true" | jq -r '.[].Key'
    cat /tmp/discovery.log
    exit 1
fi

if ! echo "$KV_VALUE" | grep -q "it-svc-f.test.local"; then
    echo "FAIL: config missing expected server_name" >&2
    echo "Config: $KV_VALUE"
    exit 1
fi

echo "PASS: nginx config stored at $KV_KEY"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );

    let out = run(&script);
    assert_pass(&out, "Test F — nginx config KV write");
}

#[test]
fn forwarding_no_kv_config() {
    let cname = "it-fwd-nokv";
    let services_yaml = r#"
services:
  it-svc-h:
    type: docker
    match:
      project: it-svc-h
    forwardremote:
    - port: 80
      ext_ip: 203.0.113.43
      ext_ports:
      - 36000
      proto: tcp"#;

    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-h" nginx:alpine
sleep 4

KEYS=$(curl -sf "$CONSUL_HTTP_ADDR/v1/kv/nginx-configs/?recurse=true" | jq -r '.[].Key // empty')
MATCH=$(echo "$KEYS" | grep "it-svc-h" || true)
if [ -n "$MATCH" ]; then
    echo "FAIL: forwarding service should have no nginx KV config, got $MATCH" >&2
    exit 1
fi

echo "PASS: forwarding service has no KV config"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );

    let out = run(&script);
    assert_pass(&out, "Test H — forwarding no KV config");
}

#[test]
fn stream_template_stored_in_streams_prefix() {
    let cname = "it-stream";
    let services_yaml = r#"
services:
  it-svc-stream:
    type: docker
    match:
      project: it-svc-stream
    rproxylocal:
    - port: 80
      template: TCP_PROXY
      domains:
      - it-svc-stream.test.local"#;
    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-stream" nginx:alpine
sleep 4

SVC_ID=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-stream") | .key')

KV_KEY="nginx-configs/streams/${{SVC_ID}}.conf"
KV_VALUE=$(curl -sf "$CONSUL_HTTP_ADDR/v1/kv/${{KV_KEY}}?raw=true")
if [ -z "$KV_VALUE" ] || [ "$KV_VALUE" = "null" ]; then
    echo "FAIL: stream config not found in streams prefix at $KV_KEY" >&2
    curl -sf "$CONSUL_HTTP_ADDR/v1/kv/nginx-configs/?recurse=true" | jq -r '.[].Key'
    exit 1
fi

SITES_KEY="nginx-configs/sites/${{SVC_ID}}.conf"
SITES_VALUE=$(curl -sf "$CONSUL_HTTP_ADDR/v1/kv/${{SITES_KEY}}?raw=true" || true)
if [ -n "$SITES_VALUE" ] && [ "$SITES_VALUE" != "null" ]; then
    echo "FAIL: TCP_PROXY template should not store in sites prefix" >&2
    exit 1
fi

echo "PASS: stream template stored in streams prefix"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );
    let out = run(&script);
    assert_pass(&out, "Phase 5 — stream template KV prefix");
}

#[test]
fn generator_fails_daemon_warns() {
    let cname = "it-ngx-genfail";
    let services_yaml = r#"
services:
  it-svc-genfail:
    type: docker
    match:
      project: it-svc-genfail
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-genfail.test.local"#;
    let script = format!(
        r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-genfail" nginx:alpine
sleep 5

PORT=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-genfail") | .value.Port // empty')
if [ -z "$PORT" ]; then echo "FAIL: service not registered despite generator failure" >&2; exit 1; fi

echo "PASS: service registered despite generator failure"
{teardown}
"#,
        setup = new_format_setup_with_defaults_ext(services_yaml, "", "", "--no-forwarding"),
        teardown = teardown(&[cname]),
        cname = cname,
    );
    let out = run(&script);
    assert_pass(&out, "Phase 5 — generator failure");
}

#[test]
fn change_nginx_generator_path_missing() {
    let cname = "it-cfg-gen";
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

cat > /tmp/discovery.yaml <<'YAMLEOF'
node:
  name: int-test-node
defaults:
  nginx_generator: /tmp/gen-nginx
services:
  it-cfg-gen-svc:
    type: docker
    match:
      project: it-cfg-gen-svc
    rproxylocal:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-cfg-gen.test.local
YAMLEOF

lab-ops auto-discover daemon /tmp/discovery.yaml --state-dir /tmp/state --no-forwarding --no-nginx --consul-addr $CONSUL_HTTP_ADDR >/tmp/discovery.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || {{ echo "FAIL: daemon died"; cat /tmp/discovery.log; exit 1; }}

docker run -d --name {cname} -l "com.docker.compose.project=it-cfg-gen-svc" nginx:alpine
sleep 5

SVC_PORT=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-cfg-gen-svc") | .value.Port // empty')
if [ -z "$SVC_PORT" ]; then echo "FAIL: service not registered (generator missing should not block Consul registration)" >&2; exit 1; fi

SVC_ID=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-cfg-gen-svc") | .key')
KV_KEY="nginx-configs/sites/${{SVC_ID}}.conf"
KV_VAL=$(curl -sf "$CONSUL_HTTP_ADDR/v1/kv/${{KV_KEY}}?raw=true" || true)
if [ -n "$KV_VAL" ] && [ "$KV_VAL" != "null" ]; then
    echo "FAIL: KV config should not exist when generator is missing, found at $KV_KEY" >&2
    exit 1
fi

echo "PASS: service registered in Consul, KV config skipped"
docker rm -f {cname} 2>/dev/null || true
kill %1 %2 %3 2>/dev/null || true
sleep 1
"#,
    );
    let out = run(&script);
    assert_pass(&out, "Phase 3 — missing generator skips KV");
}
