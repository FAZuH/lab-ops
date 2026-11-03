//! The `natmap plan` / `apply` exit contract, through the real binary.
//!
//! A plan writes nothing and exits non-zero on drift; `apply --dry-run` does
//! the same. That is a promise about a process exit code, which no unit test
//! can make: these tests spawn `lab-ops` against a Unix socket that answers the
//! one read the command needs, and assert both the output and what the command
//! sent.

use std::io::Read;
use std::io::Write;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// A `GET /mappings` answer with no mappings at all.
const NO_MAPPINGS: &str = r#"{"docker":[],"dnats":[],"snats":[],"hairpins":[],"policy_routes":[]}"#;

/// A `GET /mappings` answer holding the mapping the declaration asks for.
const DECLARED_MAPPING: &str = r#"{"docker":[{"id":1,"request":{"host_addr":"100.64.0.17:39001","container_addr":"127.0.0.1:39001","proto":"tcp"},"container_id":"service:fazuh-uptime","container_name":"fazuh-uptime","rule_comment":"natmap:service:fazuh-uptime:39001","owner":"fazuh-uptime"}],"dnats":[],"snats":[],"hairpins":[],"policy_routes":[]}"#;

/// A Unix socket that answers every request with one canned body.
struct FakeDaemon {
    dir: tempfile::TempDir,
    socket: PathBuf,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    server: Option<std::thread::JoinHandle<()>>,
}

impl FakeDaemon {
    fn start(body: &'static str) -> FakeDaemon {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("natmap.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();

        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let seen = requests.clone();
        let stopping = stop.clone();
        let server = std::thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => answer(&mut stream, body, &seen),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });

        FakeDaemon {
            dir,
            socket,
            requests,
            stop,
            server: Some(server),
        }
    }

    /// Every request line the daemon was sent, in order.
    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for FakeDaemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

/// Reads one HTTP request head, records its request line, writes `body`.
fn answer(stream: &mut std::os::unix::net::UnixStream, body: &str, seen: &Mutex<Vec<String>>) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut request = Vec::new();
    let mut buf = [0u8; 1024];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => request.extend_from_slice(&buf[..n]),
        }
    }
    let head = String::from_utf8_lossy(&request);
    let line = head.lines().next().unwrap_or_default().to_string();
    seen.lock().unwrap().push(line);

    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

/// Writes a declaration giving one port to `service`, and returns its path.
fn declaration(dir: &tempfile::TempDir, service: &str, port: u16) -> PathBuf {
    let path = dir.path().join("discovery.yaml");
    std::fs::write(
        &path,
        format!(
            "node:\n  name: test-node\n  tailnet_ip: 100.64.0.17\n\
             services:\n  {service}:\n    type: docker\n    ports:\n      - port: {port}\n    publish: [{port}]\n"
        ),
    )
    .unwrap();
    path
}

/// Runs `lab-ops natmap --socket <socket> <args...>`.
fn run(socket: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lab-ops"))
        .arg("natmap")
        .arg("--socket")
        .arg(socket)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn plan_exits_non_zero_and_prints_what_it_would_create() {
    let daemon = FakeDaemon::start(NO_MAPPINGS);
    let from = declaration(&daemon.dir, "fazuh-uptime", 39001);

    let output = run(&daemon.socket, &["plan", "--from", from.to_str().unwrap()]);

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert_eq!(output.status.code(), Some(1), "stdout: {stdout}");
    assert!(stdout.contains("create"), "{stdout}");
    assert!(stdout.contains("100.64.0.17:39001"), "{stdout}");
    assert!(stdout.contains("drift: 1 to create"), "{stdout}");
}

#[test]
fn plan_exits_zero_when_the_node_matches_its_declaration() {
    let daemon = FakeDaemon::start(DECLARED_MAPPING);
    let from = declaration(&daemon.dir, "fazuh-uptime", 39001);

    let output = run(&daemon.socket, &["plan", "--from", from.to_str().unwrap()]);

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert_eq!(output.status.code(), Some(0), "stdout: {stdout}");
    assert!(stdout.contains("converged"), "{stdout}");
}

#[test]
fn apply_dry_run_writes_nothing_and_exits_non_zero_on_drift() {
    let daemon = FakeDaemon::start(NO_MAPPINGS);
    let from = declaration(&daemon.dir, "fazuh-uptime", 39001);

    let output = run(
        &daemon.socket,
        &["apply", "--from", from.to_str().unwrap(), "--dry-run"],
    );

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert_eq!(output.status.code(), Some(1), "stdout: {stdout}");
    assert!(stdout.contains("create"), "{stdout}");
    let requests = daemon.requests();
    assert_eq!(requests.len(), 1, "a dry run must only read: {requests:?}");
    assert!(requests[0].starts_with("GET "), "{requests:?}");
}

#[test]
fn apply_prints_the_conflict_row_before_refusing_it() {
    let daemon = FakeDaemon::start(DECLARED_MAPPING);
    let from = declaration(&daemon.dir, "fazuh-grafana", 39001);

    let output = run(&daemon.socket, &["apply", "--from", from.to_str().unwrap()]);

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stdout.contains("conflict"),
        "the row must be printed: {stdout}"
    );
    assert!(
        stderr.contains("fazuh-uptime") && stderr.contains("fazuh-grafana"),
        "the refusal must name both services: {stderr}"
    );
}

#[test]
fn apply_writes_nothing_when_the_node_already_converged() {
    let daemon = FakeDaemon::start(DECLARED_MAPPING);
    let from = declaration(&daemon.dir, "fazuh-uptime", 39001);

    let output = run(
        &daemon.socket,
        &["apply", "--from", from.to_str().unwrap(), "--prune"],
    );

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert_eq!(output.status.code(), Some(0), "stdout: {stdout}");
    assert!(stdout.contains("converged"), "{stdout}");
    let requests = daemon.requests();
    assert_eq!(
        requests.len(),
        1,
        "a converged apply must only read: {requests:?}"
    );
    assert!(requests[0].starts_with("GET "), "{requests:?}");
}
