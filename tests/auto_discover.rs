#[cfg(feature = "docker-tests")]
mod integration_tests {
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

    /// Shared setup preamble.
    fn test_setup(yaml_body: &str, extra_setup: &str) -> String {
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
name: int-test-node
defaults:
  nginx_generator: /tmp/gen-nginx
{yaml_body}
YAMLEOF

cat > /tmp/gen-nginx <<'GENEOF'
#!/bin/bash
cat <<EOF
# Service: ${{LAB_DISCOVERY_SERVICE_NAME:-unknown}}
server {{
    server_name ${{LAB_DISCOVERY_DOMAIN:-_}};
    listen ${{LAB_DISCOVERY_PROXY_IP:-__TAILSCALE_IP__}}:80;
}}
EOF
GENEOF
chmod +x /tmp/gen-nginx

{extra_setup}

lab-ops auto-discover daemon /tmp/discovery.yaml \
    --state-dir /tmp/state \
    --no-forwarding \
    --no-nginx \
    --consul-addr http://127.0.0.1:8500 \
    >/tmp/discovery.log 2>&1 &
sleep 2
if ! kill -0 $! 2>/dev/null; then echo "FAIL: auto-discover daemon died" >&2; cat /tmp/discovery.log; exit 1; fi
"#
        )
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

    // ── Test A: Default Binding (0.0.0.0) ────────────────────────────

    #[test]
    fn default_binding_all_interfaces() {
        let cname = "it-def-bind";
        let yaml = r#"
networks:
  - name: it-svc-a
    container_port: 80
    template: REVERSE_PROXY
    domains:
      - it-svc-a.test.local
"#;

        let script = format!(
            r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-a" nginx:alpine
sleep 4

PORT=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-a") | .value.Port')
if [ -z "$PORT" ] || [ "$PORT" = "null" ]; then echo "FAIL: not registered with Consul" >&2; cat /tmp/discovery.log; exit 1; fi

CID=$(docker inspect -f '{{{{.Id}}}}' {cname} | cut -c1-12)
MAPPING=$(lab-ops natmap --socket /tmp/natmap.sock ls | awk -v id="$CID" '$6 == id {{print $8}}')
EXPECTED="0.0.0.0:$PORT"
if [ "$MAPPING" != "$EXPECTED" ]; then echo "FAIL: expected $EXPECTED, got $MAPPING" >&2; exit 1; fi

echo "PASS: bound to $EXPECTED"
{teardown}
"#,
            setup = test_setup(yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );

        let out = run(&script);
        assert_pass(&out, "Test A — default binding");
    }

    // ── Test B: Strict IP Binding (bind_ip) ──────────────────────────

    #[test]
    fn bind_ip_strict_address() {
        let cname = "it-bind-ip";
        let yaml = r#"
networks:
  - name: it-svc-b
    container_port: 80
    bind_ip: 10.99.99.1
    template: REVERSE_PROXY_PRIVATE
    domains:
      - it-svc-b.test.local
"#;

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
            setup = test_setup(yaml, ""),
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
        let yaml = r#"
networks:
  - name: it-svc-c
    container_port: 80
    bind_interface: dummy0
    template: REVERSE_PROXY
    domains:
      - it-svc-c.test.local
"#;

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
            setup = test_setup(yaml, ""),
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
        let yaml = r#"
networks:
  - name: it-svc-d
    container_port: 80
    forwarding:
      ext_ip: 203.0.113.43
      ext_ports: [36000]
      proto: tcp
"#;

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
            setup = test_setup(yaml, ""),
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
        let yaml = r#"
networks:
  - name: it-svc-e
    container_port: 80
    forwarding:
      ext_ip: 203.0.113.43
      ext_ports: [36001]
      proto: tcp
      hairpin: true
"#;

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
            setup = test_setup(yaml, ""),
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
        let yaml = r#"
networks:
  - name: it-svc-f
    container_port: 80
    template: REVERSE_PROXY
    domains:
      - it-svc-f.test.local
"#;

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
            setup = test_setup(yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );

        let out = run(&script);
        assert_pass(&out, "Test F — nginx config KV write");
    }

    // ── Test G: Private Service Gets TAILSCALE Placeholder ───────────

    #[test]
    fn nginx_config_private_service_placeholder() {
        let cname = "it-ngx-priv";
        let yaml = r#"
networks:
  - name: it-svc-g
    container_port: 80
    bind_ip: 10.99.99.1
    template: REVERSE_PROXY_PRIVATE
    domains:
      - it-svc-g.test.local
"#;

        let script = format!(
            r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-g" nginx:alpine
sleep 4

SVC_ID=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-g") | .key')
KV_KEY="nginx-configs/sites/${{SVC_ID}}.conf"
KV_VALUE=$(curl -sf "$CONSUL_HTTP_ADDR/v1/kv/${{KV_KEY}}?raw=true")

if ! echo "$KV_VALUE" | grep -q '__TAILSCALE_IP__'; then
    echo "FAIL: private service config missing __TAILSCALE_IP__ placeholder" >&2
    echo "Config: $KV_VALUE"
    exit 1
fi

echo "PASS: private service has __TAILSCALE_IP__ placeholder"
{teardown}
"#,
            setup = test_setup(yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );

        let out = run(&script);
        assert_pass(&out, "Test G — private service placeholder");
    }

    // ── Test H: Forwarding Service Has No KV Config ──────────────────

    #[test]
    fn forwarding_no_kv_config() {
        let cname = "it-fwd-nokv";
        let yaml = r#"
networks:
  - name: it-svc-h
    container_port: 80
    forwarding:
      ext_ip: 203.0.113.43
      ext_ports: [36000]
      proto: tcp
"#;

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
            setup = test_setup(yaml, ""),
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
        let yaml = r#"
networks:
  - name: it-svc-i
    container_port: 80
    template: REVERSE_PROXY_PRIVATE
    domains:
      - it-svc-i.test.local
"#;

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
            setup = test_setup(yaml, ""),
            cname = cname,
        );

        let out = run(&script);
        assert_pass(&out, "Test I — KV delete + deregister on stop");
    }

    // ══════════════════════════════════════════════════════════════════
    // Phase 2 — Crash Recovery
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn restart_auto_discover_picks_up_missed_containers() {
        let cname = "it-restart-ad";
        let yaml_body = r#"
networks:
  - name: it-svc-restart
    container_port: 80
    template: REVERSE_PROXY_PRIVATE
    domains:
      - it-svc-restart.test.local
"#;
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
name: int-test-node
defaults:
  nginx_generator: /tmp/gen-nginx
{yaml_body}
YAMLEOF

cat > /tmp/gen-nginx <<'GENEOF'
#!/bin/bash
cat <<EOF
# Service: ${{LAB_DISCOVERY_SERVICE_NAME:-unknown}}
server {{
    server_name ${{LAB_DISCOVERY_DOMAIN:-_}};
    listen ${{LAB_DISCOVERY_PROXY_IP:-__TAILSCALE_IP__}}:80;
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
        let yaml_body = r#"
networks:
  - name: it-svc-nmrestart
    container_port: 80
    template: REVERSE_PROXY_PRIVATE
    domains:
      - it-svc-nmrestart.test.local
"#;
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
name: int-test-node
defaults:
  nginx_generator: /tmp/gen-nginx
{yaml_body}
YAMLEOF

cat > /tmp/gen-nginx <<'GENEOF'
#!/bin/bash
cat <<EOF
# Service: ${{LAB_DISCOVERY_SERVICE_NAME:-unknown}}
server {{
    server_name ${{LAB_DISCOVERY_DOMAIN:-_}};
    listen ${{LAB_DISCOVERY_PROXY_IP:-__TAILSCALE_IP__}}:80;
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
    fn host_networked_container_skipped() {
        let cname = "it-host-net";
        let yaml = r#"
networks:
  - name: it-svc-hostnet
    container_port: 80
    template: REVERSE_PROXY_PRIVATE
    domains:
      - it-svc-hostnet.test.local
"#;
        let script = format!(
            r#"{setup}
docker run -d --name {cname} --network host -l "com.docker.compose.project=it-svc-hostnet" nginx:alpine
sleep 4

SVC=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-hostnet") | .value.Port // empty')
if [ -n "$SVC" ]; then echo "FAIL: host-networked container should not be registered, got port=$SVC" >&2; exit 1; fi

echo "PASS: host-networked container correctly skipped"
{teardown}
"#,
            setup = test_setup(yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );
        let out = run(&script);
        assert_pass(&out, "Phase 4 — host-networked skip");
    }

    #[test]
    fn wrong_exposed_port_skipped() {
        let cname = "it-wrong-port";
        let yaml = r#"
networks:
  - name: it-svc-wrongport
    container_port: 9999
    template: REVERSE_PROXY_PRIVATE
    domains:
      - it-svc-wrongport.test.local
"#;
        let script = format!(
            r#"{setup}
docker run -d --name {cname} -l "com.docker.compose.project=it-svc-wrongport" nginx:alpine
sleep 4

SVC=$(curl -sf $CONSUL_HTTP_ADDR/v1/agent/services | jq -r 'to_entries[] | select(.value.Service == "it-svc-wrongport") | .value.Port // empty')
if [ -n "$SVC" ]; then echo "FAIL: mismatched port container should not be registered, got port=$SVC" >&2; exit 1; fi

echo "PASS: wrong exposed port correctly skipped"
{teardown}
"#,
            setup = test_setup(yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );
        let out = run(&script);
        assert_pass(&out, "Phase 4 — wrong port skip");
    }

    // ══════════════════════════════════════════════════════════════════
    // Phase 5 — Nginx Config Generation
    // ══════════════════════════════════════════════════════════════════

    #[test]
    fn stream_template_stored_in_streams_prefix() {
        let cname = "it-stream";
        let yaml = r#"
networks:
  - name: it-svc-stream
    container_port: 80
    template: STREAM
    domains:
      - it-svc-stream.test.local
"#;
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
    echo "FAIL: STREAM template should not store in sites prefix" >&2
    exit 1
fi

echo "PASS: stream template stored in streams prefix"
{teardown}
"#,
            setup = test_setup(yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );
        let out = run(&script);
        assert_pass(&out, "Phase 5 — stream template KV prefix");
    }

    #[test]
    fn extra_fields_passed_to_consul_meta() {
        let cname = "it-extra";
        let yaml = r#"
networks:
  - name: it-svc-extra
    container_port: 80
    template: REVERSE_PROXY_PRIVATE
    domains:
      - it-svc-extra.test.local
    extra:
      cluster: "us-east"
      max_conns: "100"
"#;
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
            setup = test_setup(yaml, ""),
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
        let yaml = r#"
networks:
  - name: it-svc-slug
    container_port: 80
    template: REVERSE_PROXY_PRIVATE
    domains:
      - it.svc.slug.test.local
"#;
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
            setup = test_setup(yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );
        let out = run(&script);
        assert_pass(&out, "Phase 6 — domain slug in service ID");
    }

    #[test]
    fn service_id_no_domain_falls_back_to_name() {
        let cname = "it-nodomain";
        let yaml = r#"
networks:
  - name: it-svc-nodomain
    container_port: 80
    template: REVERSE_PROXY_PRIVATE
"#;
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
            setup = test_setup(yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );
        let out = run(&script);
        assert_pass(&out, "Phase 6 — no-domain fallback ID");
    }

    #[test]
    fn container_id_in_consul_meta() {
        let cname = "it-cid-meta";
        let yaml = r#"
networks:
  - name: it-svc-cidmeta
    container_port: 80
    template: REVERSE_PROXY_PRIVATE
    domains:
      - it-svc-cidmeta.test.local
"#;
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
            setup = test_setup(yaml, ""),
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
        let yaml = r#"
networks:
  - name: it-svc-reuse
    container_port: 80
    template: REVERSE_PROXY_PRIVATE
    domains:
      - it-svc-reuse.test.local
"#;
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
            setup = test_setup(yaml, ""),
            teardown = test_teardown(&[cname]),
            cname = cname,
        );
        let out = run(&script);
        assert_pass(&out, "Phase 9 — port reuse on restart");
    }
}
