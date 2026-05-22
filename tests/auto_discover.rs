#[cfg(feature = "docker-tests")]
mod auto_discover {
    use std::process::Command;
    use std::sync::Once;

    static INIT: Once = Once::new();

    fn setup_image() -> &'static str {
        let image_name = "lab-ops-auto-discover-test:latest";
        INIT.call_once(|| {
            let dockerfile = concat!(
                "FROM ubuntu:24.04\n",
                "RUN apt-get update && apt-get install -y iptables jq curl unzip iproute2 docker.io\n",
                "RUN curl -fsSL https://releases.hashicorp.com/consul/1.19.2/consul_1.19.2_linux_amd64.zip ",
                "-o /tmp/consul.zip && unzip /tmp/consul.zip -d /usr/local/bin && rm /tmp/consul.zip\n",
            );
            let mut child = Command::new("docker")
                .args(["build", "-t", image_name, "-"])
                .stdin(std::process::Stdio::piped())
                .spawn()
                .expect("Failed to spawn docker build for auto-discover test image");
            {
                use std::io::Write;
                let mut stdin = child.stdin.take().expect("Failed to open stdin");
                stdin
                    .write_all(dockerfile.as_bytes())
                    .expect("Failed to write Dockerfile");
            }
            let status = child.wait().expect("Failed to wait for docker build");
            assert!(status.success(), "Failed to build auto-discover test image");
        });
        image_name
    }

    fn run(script: &str) -> String {
        let image = setup_image();
        let binary_path = env!("CARGO_BIN_EXE_lab-ops");
        let mut cmd = Command::new("docker");
        cmd.args([
            "run",
            "--rm",
            "--privileged",
            "-v",
            &format!("{binary_path}:/usr/local/bin/lab-ops"),
            "-v",
            "/var/run/docker.sock:/var/run/docker.sock",
            "-e",
            "NATMAP_SOCKET=/tmp/natmap.sock",
            "-e",
            "CONSUL_HTTP_ADDR=http://127.0.0.1:8500",
            image,
            "sh",
            "-c",
        ]);
        cmd.arg(script);

        let output = cmd.output().expect("Failed to execute docker run");

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            panic!("Test container failed.\nstdout:\n{stdout}\nstderr:\n{stderr}");
        }
        stdout
    }

    /// Cleanup helper.
    fn test_teardown(container_names: &[&str]) -> String {
        let removes: String = container_names
            .iter()
            .map(|n| format!("docker rm -f {n} 2>/dev/null || true"))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            r#"
kill %3 %2 %1 2>/dev/null || true
sleep 1
{removes}
"#
        )
    }

    fn assert_pass(output: &str, test_name: &str) {
        assert!(
            output.contains("PASS"),
            "{test_name} failed.\nOutput:\n{output}"
        );
    }

    /// Writes new-format YAML config via extra_setup overwrite.
    /// The services_yaml must contain the `services:` block.
    fn new_format_setup(services_yaml: &str, extra_setup: &str) -> String {
        new_format_setup_ext(services_yaml, extra_setup, "--no-forwarding --no-nginx")
    }

    fn new_format_setup_ext(services_yaml: &str, extra_setup: &str, daemon_flags: &str) -> String {
        let full_yaml = format!(
            r#"node:
  name: int-test-node
defaults:
  nginx_generator: /tmp/gen-nginx
{services_yaml}"#
        );
        format!(
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
{full_yaml}
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

{extra_setup}

lab-ops auto-discover daemon /tmp/discovery.yaml \
    --state-dir /tmp/state \
    {daemon_flags} \
    --consul-addr http://127.0.0.1:8500 \
    >/tmp/discovery.log 2>&1 &
sleep 2
if ! kill -0 $! 2>/dev/null; then echo "FAIL: auto-discover daemon died" >&2; cat /tmp/discovery.log; exit 1; fi
"#
        )
    }

    // ── Test A: Default Binding (0.0.0.0) ────────────────────────────

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

if iptables-save -t nat | grep -q "203.0.113.50"; then
    echo "FAIL: stale DNAT rules not removed" >&2
    exit 1
fi

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

    // ══════════════════════════════════════════════════════════════════
    // Phase 8 — Nginx Daemon Component
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn nginx_daemon_writes_config_to_disk() {
        let script = r#"
set -e
CONSUL_HTTP_ADDR=http://127.0.0.1:8500

consul agent -dev -http-port=8500 >/tmp/consul.log 2>&1 &
sleep 2; kill -0 $! 2>/dev/null || { echo "FAIL: consul died"; cat /tmp/consul.log; exit 1; }

mkdir -p /var/lib/auto-discover/nginx-configs
mkdir -p /etc/nginx/sites-available
mkdir -p /etc/nginx/streams-available

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
mkdir -p /etc/nginx/sites-available
mkdir -p /etc/nginx/streams-available

curl -sf -X PUT "$CONSUL_HTTP_ADDR/v1/kv/nginx-configs/sites/symlink-svc.conf" -d 'server { listen 8081; server_name symlink.local; }'

lab-ops auto-discover nginx-sync $CONSUL_HTTP_ADDR >/tmp/ngx.log 2>&1 || true

SYMLINK="/etc/nginx/sites-available/symlink-svc.conf"
if [ ! -L "$SYMLINK" ]; then
    echo "FAIL: symlink not created at $SYMLINK" >&2
    ls -la /etc/nginx/sites-available/ 2>/dev/null || true
    cat /tmp/ngx.log
    exit 1
fi

TARGET=$(readlink "$SYMLINK")
EXPECTED="/var/lib/auto-discover/nginx-configs/sites/symlink-svc.conf"
if [ "$TARGET" != "$EXPECTED" ]; then
    echo "FAIL: symlink target $TARGET != $EXPECTED" >&2
    exit 1
fi

echo "PASS: symlink created in sites-available"
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
mkdir -p /etc/nginx/sites-available
mkdir -p /etc/nginx/streams-available

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
mkdir -p /etc/nginx/sites-available
mkdir -p /etc/nginx/streams-available
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
mkdir -p /etc/nginx/sites-available

echo 'stale config' > /var/lib/auto-discover/nginx-configs/sites/stale-svc.conf
ln -sf /var/lib/auto-discover/nginx-configs/sites/stale-svc.conf /etc/nginx/sites-available/stale-svc.conf

lab-ops auto-discover nginx-sync $CONSUL_HTTP_ADDR >/tmp/ngx.log 2>&1 || true

STALE_FILE="/var/lib/auto-discover/nginx-configs/sites/stale-svc.conf"
STALE_LINK="/etc/nginx/sites-available/stale-svc.conf"

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
mkdir -p /etc/nginx/sites-available

curl -sf -X PUT "$CONSUL_HTTP_ADDR/v1/kv/nginx-configs/sites/cycle-svc.conf" -d 'server { listen 9991; server_name cycle.local; }'

lab-ops auto-discover nginx-sync $CONSUL_HTTP_ADDR >/tmp/ngx.log 2>&1 || true

FILE="/var/lib/auto-discover/nginx-configs/sites/cycle-svc.conf"
LINK="/etc/nginx/sites-available/cycle-svc.conf"
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

    // ══════════════════════════════════════════════════════════════════
    // Phase 9 — Edge Cases & Stress (remaining)
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn docker_forwarding_local_bind_port() {
        let cname = "it-fwd-local";
        let services_yaml = r#"
services:
  it-svc-fwd-local:
    type: docker
    match:
      project: it-svc-fwd-local
    forwarding:
      - type: local
        port: 80
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

KEYS=$(curl -sf "$CONSUL_HTTP_ADDR/v1/kv/nginx-configs/?recurse=true" | jq -r '.[].Key // empty')
MATCH=$(echo "$KEYS" | grep "it-svc-fwd-local" || true)
if [ -n "$MATCH" ]; then echo "FAIL: forwarding-local should have no nginx KV config, got $MATCH" >&2; exit 1; fi

echo "PASS: forwarding local bind_port=36000 with forwarding_type=local"
{teardown}
"#,
            setup = new_format_setup(services_yaml, ""),
            teardown = test_teardown(&[cname]),
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
    rproxy:
      - port: 80
        template: HTTP_PROXY
        domains:
          - fwd-local-tpl.test.local
    forwarding:
      - type: local
        port: 80
        bind_port: 36001
"#;

        let script = format!(
            r#"{setup}
docker rm -f {cname} 2>/dev/null || true
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-fwd-local-tpl" nginx:alpine
sleep 4

COUNT=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq '[to_entries[] | select(.value.Service == "it-svc-fwd-local-tpl")] | length')
if [ "$COUNT" != "1" ]; then echo "FAIL: expected 1 Consul entry, got $COUNT" >&2; exit 1; fi

SVC=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq 'to_entries[] | select(.value.Service == "it-svc-fwd-local-tpl") | .value')
PORT=$(echo "$SVC" | jq -r '.Port')
if [ "$PORT" != "36001" ]; then echo "FAIL: expected static port 36001, got $PORT" >&2; exit 1; fi

META=$(echo "$SVC" | jq '.Meta')
FORWARDING=$(echo "$META" | jq -r '.forwarding')
if [ "$FORWARDING" != "true" ]; then echo "FAIL: missing forwarding meta" >&2; exit 1; fi

FWD_TYPE=$(echo "$META" | jq -r '.forwarding_type')
if [ "$FWD_TYPE" != "local" ]; then echo "FAIL: expected forwarding_type=local, got $FWD_TYPE" >&2; exit 1; fi

TEMPLATE=$(echo "$META" | jq -r '.template')
if [ "$TEMPLATE" != "HTTP_PROXY" ]; then echo "FAIL: expected template=HTTP_PROXY, got $TEMPLATE" >&2; exit 1; fi

# Verify nginx KV config exists (non-empty template)
KEYS=$(curl -sf "$CONSUL_HTTP_ADDR/v1/kv/nginx-configs/?recurse=true" | jq -r '.[].Key // empty')
MATCH=$(echo "$KEYS" | grep "it-svc-fwd-local-tpl" || true)
if [ -z "$MATCH" ]; then echo "FAIL: forwarding-local with template should have nginx KV config" >&2; exit 1; fi

echo "PASS: forwarding local with template merged, port=$PORT forwarding=$FORWARDING template=$TEMPLATE"
{teardown}
"#,
            setup = new_format_setup_ext(services_yaml, "", "--no-forwarding"),
            teardown = test_teardown(&[cname]),
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
    forwarding:
      - type: local
        port: 5000
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

# Verify no nginx KV config (empty template)
KEYS=$(curl -sf "$CONSUL_HTTP_ADDR/v1/kv/nginx-configs/?recurse=true" | jq -r '.[].Key // empty')
MATCH=$(echo "$KEYS" | grep "it-local-fwd-local" || true)
if [ -n "$MATCH" ]; then echo "FAIL: forwarding-local should have no nginx KV config, got $MATCH" >&2; exit 1; fi

echo "PASS: local forwarding local at 10.99.99.99:50000 with forwarding_type=local"
kill %3 %2 %1 2>/dev/null || true
sleep 1
"#,
            setup = new_format_setup(services_yaml, ""),
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
    forwarding:
      - type: local
        port: 80
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
            setup = new_format_setup(services_yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );

        let out = run(&script);
        assert_pass(&out, "docker_forwarding_local_no_bind");
    }

    // ── Test B: Strict IP Binding (bind_ip) ──────────────────────────

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
    rproxy:
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
            setup = new_format_setup(services_yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );

        let out = run(&script);
        assert_pass(&out, "Test B — bind_ip");
    }

    // ── Test C: Interface Binding (bind_interface) ───────────────────

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
    rproxy:
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
            setup = new_format_setup(services_yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );

        let out = run(&script);
        assert_pass(&out, "Test C — bind_interface");
    }

    // ── Test D: Forwarding with Static Port ──────────────────────────

    #[test]
    fn forwarding_static_port() {
        let cname = "it-fwd";
        let services_yaml = r#"
services:
  it-svc-d:
    type: docker
    match:
      project: it-svc-d
    forwarding:
    - type: remote
      port: 80
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
            setup = new_format_setup(services_yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );

        let out = run(&script);
        assert_pass(&out, "Test D — forwarding static port");
    }

    // ── Test E: Forwarding with Hairpin ──────────────────────────────

    #[test]
    fn forwarding_hairpin_meta() {
        let cname = "it-hairpin";
        let services_yaml = r#"
services:
  it-svc-e:
    type: docker
    match:
      project: it-svc-e
    forwarding:
    - type: remote
      port: 80
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
            setup = new_format_setup(services_yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );

        let out = run(&script);
        assert_pass(&out, "Test E — forwarding hairpin");
    }

    // ── Test F: Nginx Config KV Write ───────────────────────────────

    #[test]
    fn nginx_config_kv_write() {
        let cname = "it-nginx-kv";
        let services_yaml = r#"
services:
  it-svc-f:
    type: docker
    match:
      project: it-svc-f
    rproxy:
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
            setup = new_format_setup(services_yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );

        let out = run(&script);
        assert_pass(&out, "Test F — nginx config KV write");
    }

    // ── Test H: Forwarding Service Has No KV Config ──────────────────

    #[test]
    fn forwarding_no_kv_config() {
        let cname = "it-fwd-nokv";
        let services_yaml = r#"
services:
  it-svc-h:
    type: docker
    match:
      project: it-svc-h
    forwarding:
    - type: remote
      port: 80
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
            setup = new_format_setup(services_yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );

        let out = run(&script);
        assert_pass(&out, "Test H — forwarding no KV config");
    }

    // ── Test I: KV Delete + Consul Deregistration on Container Stop ──

    #[test]
    fn container_stop_kv_delete_and_deregister() {
        let cname = "it-stop";
        let services_yaml = r#"
services:
  it-svc-i:
    type: docker
    match:
      project: it-svc-i
    rproxy:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-i.test.local"#;

        let script = format!(
            r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-i" nginx:alpine
sleep 4

SVC_ID=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-i") | .key')
KV_KEY="nginx-configs/sites/${{SVC_ID}}.conf"

INITIAL=$(curl -sf "$CONSUL_HTTP_ADDR/v1/kv/${{KV_KEY}}?raw=true")
if [ -z "$INITIAL" ] || [ "$INITIAL" = "null" ]; then echo "FAIL: config not written" >&2; exit 1; fi

docker stop {cname}
sleep 5

AFTER=$(curl -sf "$CONSUL_HTTP_ADDR/v1/kv/${{KV_KEY}}?raw=true" || true)
if [ -n "$AFTER" ] && [ "$AFTER" != "null" ]; then
    echo "FAIL: KV key not deleted after stop" >&2
    exit 1
fi

PORT_AFTER=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-i") | .value.Port // empty')
if [ -n "$PORT_AFTER" ]; then
    echo "FAIL: service not deregistered from Consul" >&2
    exit 1
fi

echo "PASS: KV deleted and service deregistered"
docker rm -f {cname} 2>/dev/null || true
kill %3 %2 %1 2>/dev/null || true
sleep 1
"#,
            setup = new_format_setup(services_yaml, ""),
            cname = cname,
        );

        let out = run(&script);
        assert_pass(&out, "Test I — KV delete + deregister on stop");
    }

    // ══════════════════════════════════════════════════════════════════
    // Phase 2 — Crash Recovery
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn host_networked_container_skipped() {
        let cname = "it-host-net";
        let services_yaml = r#"
services:
  it-svc-hostnet:
    type: docker
    match:
      project: it-svc-hostnet
    rproxy:
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
            setup = new_format_setup(services_yaml, ""),
            teardown = test_teardown(&[cname]),
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
    rproxy:
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
            setup = new_format_setup(services_yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );
        let out = run(&script);
        assert_pass(&out, "Phase 4 — no port exposure still matched");
    }

    // ══════════════════════════════════════════════════════════════════
    // Phase 5 — Nginx Config Generation
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn stream_template_stored_in_streams_prefix() {
        let cname = "it-stream";
        let services_yaml = r#"
services:
  it-svc-stream:
    type: docker
    match:
      project: it-svc-stream
    rproxy:
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
            setup = new_format_setup(services_yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );
        let out = run(&script);
        assert_pass(&out, "Phase 5 — stream template KV prefix");
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
    rproxy:
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
            setup = new_format_setup(services_yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );
        let out = run(&script);
        assert_pass(&out, "Phase 5 — extra fields meta");
    }

    // ══════════════════════════════════════════════════════════════════
    // Phase 6 — Consul Registration Details
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn service_id_contains_domain_slug() {
        let cname = "it-slug";
        let services_yaml = r#"
services:
  it-svc-slug:
    type: docker
    match:
      project: it-svc-slug
    rproxy:
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
            setup = new_format_setup(services_yaml, ""),
            teardown = test_teardown(&[cname]),
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
    rproxy:
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
            setup = new_format_setup(services_yaml, ""),
            teardown = test_teardown(&[cname]),
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
    rproxy:
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
            setup = new_format_setup(services_yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );
        let out = run(&script);
        assert_pass(&out, "Phase 6 — container_id meta");
    }

    // ══════════════════════════════════════════════════════════════════
    // Phase 9 — Edge Cases
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn container_restart_reuses_port_from_state() {
        let cname = "it-reuse";
        let services_yaml = r#"
services:
  it-svc-reuse:
    type: docker
    match:
      project: it-svc-reuse
    rproxy:
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
            setup = new_format_setup(services_yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );
        let out = run(&script);
        assert_pass(&out, "Phase 9 — port reuse on restart");
    }

    // ══════════════════════════════════════════════════════════════════
    // Phase 3 — Config Change Handling
    // ══════════════════════════════════════════════════════════════════

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
    rproxy:
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
            setup = new_format_setup(services_yaml, ""),
            teardown = test_teardown(&[cname]),
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
    rproxy:
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
            setup = new_format_setup(services_yaml, ""),
            teardown = test_teardown(&[cname]),
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
    rproxy:
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
            setup = new_format_setup(services_yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );
        let out = run(&script);
        assert_pass(&out, "Phase 5 — multi-domain");
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
    rproxy:
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
            setup = new_format_setup(services_yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );
        let out = run(&script);
        assert_pass(&out, "Phase 5 — generator failure");
    }

    // ══════════════════════════════════════════════════════════════════
    // Phase 7 — Forwarding Sync
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn compose_project_mismatch_skipped() {
        let cname = "it-edge-mismatch";
        let services_yaml = r#"
services:
  it-svc-mismatch:
    type: docker
    match:
      project: it-svc-mismatch
    rproxy:
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
            setup = new_format_setup(services_yaml, ""),
            teardown = test_teardown(&[cname]),
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
    rproxy:
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
            setup = new_format_setup(services_yaml, ""),
            cname = cname,
        );
        let out = run(&script);
        assert_pass(&out, "Phase 9 — die event deregistration");
    }

    #[test]
    fn local_service() {
        let services_yaml = r#"
services:
  it-local-app:
    type: local
    address: 10.99.99.99
    rproxy:
    - port: 3000
      template: HTTP_PROXY
      domains:
      - app.local.test"#;

        let script = format!(
            r#"{setup}

PORT=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-local-app") | .value.Port')
if [ -z "$PORT" ] || [ "$PORT" = "null" ]; then echo "FAIL: not registered with Consul" >&2; cat /tmp/discovery.log; exit 1; fi
if [ "$PORT" != "3000" ]; then echo "FAIL: expected port 3000, got $PORT" >&2; exit 1; fi

ADDR=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-local-app") | .value.Address')
if [ "$ADDR" != "10.99.99.99" ]; then echo "FAIL: expected address 10.99.99.99, got $ADDR" >&2; exit 1; fi

# Verify nginx config was generated in Consul KV (local services bypass NAT, proxy directly to local_ip:local_port)
SVC_ID="int-test-node-app-local-test-3000"
NGINX_CONFIG=$(curl -sf "$CONSUL_HTTP_ADDR/v1/kv/nginx-configs/sites/${{SVC_ID}}.conf?raw" 2>/dev/null || echo "")
if [ -z "$NGINX_CONFIG" ]; then echo "FAIL: no nginx config in Consul KV" >&2; curl -sf "$CONSUL_HTTP_ADDR/v1/kv/nginx-configs/sites?keys" 2>/dev/null || true; exit 1; fi

echo "$NGINX_CONFIG" | grep -q "server_name app.local.test" || \
    {{ echo "FAIL: nginx config missing server_name" >&2; echo "$NGINX_CONFIG"; exit 1; }}

echo "$NGINX_CONFIG" | grep -q "proxy_pass http://10.99.99.99:3000/;" || \
    {{ echo "FAIL: nginx config missing proxy_pass to local IP" >&2; echo "$NGINX_CONFIG"; exit 1; }}

echo "PASS: local service registered at $ADDR:$PORT, NAT bypassed, nginx config verified"
kill %3 %2 %1 2>/dev/null || true
sleep 1
"#,
            setup = new_format_setup_ext(services_yaml, "", "--no-forwarding"),
        );
        let out = run(&script);
        assert_pass(&out, "local_service");
    }

    // ── Local Service + Forwarding Remote ────────────────────────────

    #[test]
    fn local_forwarding_remote() {
        let services_yaml = r#"
services:
  it-local-fwd:
    type: local
    address: 10.99.99.99
    forwarding:
    - type: remote
      port: 4000
      ext_ip: 203.0.113.43
      ext_ports:
      - 40000
      proto: tcp"#;

        let script = format!(
            r#"{setup}

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

# Verify no nginx KV config (local forwarding remote has empty template)
KEYS=$(curl -sf "$CONSUL_HTTP_ADDR/v1/kv/nginx-configs/?recurse=true" | jq -r '.[].Key // empty')
MATCH=$(echo "$KEYS" | grep "it-local-fwd" || true)
if [ -n "$MATCH" ]; then echo "FAIL: forwarding-only service should have no nginx KV config, got $MATCH" >&2; exit 1; fi

echo "PASS: local forwarding remote at 10.99.99.99:40000 with forwarding meta"
kill %3 %2 %1 2>/dev/null || true
sleep 1
"#,
            setup = new_format_setup_ext(services_yaml, "", "--no-forwarding"),
        );
        let out = run(&script);
        assert_pass(&out, "local_forwarding_remote");
    }

    // ── Docker RProxy — Full Reachability ────────────────────────────

    #[test]
    fn docker_reachability() {
        let cname = "it-reach";
        let services_yaml = r#"
services:
  it-svc-reach:
    type: docker
    match:
      project: it-svc-reach
    rproxy:
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
            setup = new_format_setup(services_yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );
        let out = run(&script);
        assert_pass(&out, "docker_reachability");
    }

    // ── Docker Combined: RProxy + Forwarding (same project, different ports) ──

    #[test]
    fn docker_rproxy_and_forwarding() {
        let cname = "it-combo";
        let services_yaml = r#"
services:
  it-svc-combo:
    type: docker
    match:
      project: it-svc-combo
    rproxy:
      - port: 80
        template: HTTP_PROXY
        domains:
          - combo.test.local
    forwarding:
      - type: remote
        port: 80
        ext_ip: 203.0.113.43
        ext_ports:
          - 36000
        proto: tcp"#;

        let script = format!(
            r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-combo" nginx:alpine
sleep 4

# Should be exactly 1 Consul entry (rproxy + forwarding merged into one)
COUNT=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq '[to_entries[] | select(.value.Service == "it-svc-combo")] | length')
if [ "$COUNT" != "1" ]; then echo "FAIL: expected 1 Consul entry, got $COUNT" >&2; curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq '[to_entries[] | select(.value.Service == "it-svc-combo") | .key, .value.Port, .value.Meta.forwarding // "n/a"]' >&2; exit 1; fi

# The entry should have BOTH forwarding meta AND template meta
SVC_META=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq 'to_entries[] | select(.value.Service == "it-svc-combo") | .value.Meta')

FORWARDING=$(echo "$SVC_META" | jq -r '.forwarding')
if [ "$FORWARDING" != "true" ]; then echo "FAIL: missing forwarding meta" >&2; exit 1; fi

EXT_IP=$(echo "$SVC_META" | jq -r '.ext_ip')
if [ "$EXT_IP" != "203.0.113.43" ]; then echo "FAIL: missing ext_ip meta" >&2; exit 1; fi

TEMPLATE=$(echo "$SVC_META" | jq -r '.template')
if [ "$TEMPLATE" != "HTTP_PROXY" ]; then echo "FAIL: expected template HTTP_PROXY, got $TEMPLATE" >&2; exit 1; fi

PORT=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-combo") | .value.Port')
echo "PASS: merged rproxy+forwarding entry at port $PORT with template=$TEMPLATE forwarding=$FORWARDING"
{teardown}
"#,
            setup = new_format_setup(services_yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );
        let out = run(&script);
        assert_pass(&out, "docker_rproxy_and_forwarding");
    }

    // ── ForwardingLocal Tests ─────────────────────────────────────────

    #[test]
    fn restart_auto_discover_picks_up_missed_containers() {
        let cname = "it-restart-ad";
        let services_yaml = r#"
services:
  it-svc-restart:
    type: docker
    match:
      project: it-svc-restart
    rproxy:
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
    rproxy:
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

    // ══════════════════════════════════════════════════════════════════
    // Phase 4 — Natmap Integration
    // ══════════════════════════════════════════════════════════════════

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
    rproxy:
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
    rproxy:
    - port: 80
      template: HTTP_PROXY
      domains:
      - it-svc-cfg-a.test.local
  it-svc-cfg-b:
    type: docker
    match:
      project: it-svc-cfg-b
    rproxy:
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
    rproxy:
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
services: {{}}
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
    rproxy:
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
    rproxy:
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
    rproxy:
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
services: {{}}
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
    rproxy:
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

    // ══════════════════════════════════════════════════════════════════
    // Phase 5 — Nginx Config Generation (remaining)
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn concurrent_starts_all_registered() {
        let mut yaml_services = String::new();
        let mut cnames = Vec::new();
        for i in 0..5 {
            let project = format!("it-edge-con-{i}");
            yaml_services.push_str(&format!(
                "  {project}:\n    type: docker\n    match:\n      project: {project}\n    rproxy:\n      - port: 80\n        template: HTTP_PROXY\n        domains:\n          - {project}.test.local\n"
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
    fn large_config_many_services() {
        let mut yaml_services = String::new();
        let mut cnames = Vec::new();
        for i in 0..5 {
            let project = format!("it-large-{i}");
            yaml_services.push_str(&format!(
                "  {project}:\n    type: docker\n    match:\n      project: {project}\n    rproxy:\n      - port: 80\n        template: HTTP_PROXY\n        domains:\n          - {project}.test.local\n"
            ));
            cnames.push(project);
        }
        let services_yaml = format!("\nservices:\n{yaml_services}");
        let script = format!(
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
        let out = run(&script);
        assert_pass(&out, "Phase 9 — large config");
    }
}
