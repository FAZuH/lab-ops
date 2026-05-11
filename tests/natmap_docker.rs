#[cfg(feature = "docker-tests")]
mod docker_tests {
    use std::process::Command;
    use std::sync::Once;

    static INIT: Once = Once::new();

    fn setup_docker_image() -> &'static str {
        let image_name = "lab-ops-natmap-test:latest";
        INIT.call_once(|| {
            let dockerfile =
                "FROM ubuntu:24.04\nRUN apt-get update && apt-get install -y iptables\n";
            let mut cmd = Command::new("docker");
            cmd.args(["build", "-t", image_name, "-"]);

            use std::io::Write;
            let mut child = cmd
                .stdin(std::process::Stdio::piped())
                .spawn()
                .expect("Failed to spawn docker build");
            let mut stdin = child.stdin.take().expect("Failed to open stdin");
            stdin
                .write_all(dockerfile.as_bytes())
                .expect("Failed to write to stdin");
            drop(stdin);

            let status = child.wait().expect("Failed to wait for docker build");
            assert!(status.success(), "Failed to build docker image");
        });
        image_name
    }

    fn run_in_docker(args: &[&str]) -> String {
        let image = setup_docker_image();
        let binary_path = env!("CARGO_BIN_EXE_lab-ops");
        let mut cmd = Command::new("docker");
        cmd.args([
            "run",
            "--rm",
            "--privileged",
            "-v",
            &format!("{}:/usr/local/bin/lab-ops", binary_path),
            image,
            "sh",
            "-c",
        ]);

        let shell_cmd = args.join(" ");
        cmd.arg(&shell_cmd);

        let output = cmd.output().expect("Failed to execute docker run");

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            panic!("Docker command failed: {shell_cmd}\nstdout:\n{stdout}\nstderr:\n{stderr}");
        }

        String::from_utf8_lossy(&output.stdout).to_string()
    }

    #[test]
    fn test_natmap_forward() {
        let out = run_in_docker(&[
            "lab-ops natmap daemon --socket /tmp/ns --state-dir /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "lab-ops natmap --socket /tmp/ns dnat --ext-ip 1.2.3.4 --int-ip 10.0.0.1 --ports 8080",
            "&&",
            "iptables-save",
            "|",
            "grep DNAT",
        ]);
        assert!(
            out.contains("--to-destination 10.0.0.1"),
            "DNAT rule missing:\n{out}"
        );
    }

    #[test]
    fn test_natmap_snat() {
        let out = run_in_docker(&[
            "lab-ops natmap daemon --socket /tmp/ns --state-dir /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "lab-ops natmap --socket /tmp/ns snat --ext-ip 1.2.3.4 --int-ip 10.0.0.1 --ext-if eth0",
            "&&",
            "iptables-save",
            "|",
            "grep SNAT",
        ]);
        assert!(
            out.contains("SNAT --to-source 1.2.3.4"),
            "SNAT rule missing:\n{out}"
        );
    }

    #[test]
    fn test_natmap_hairpin() {
        let out = run_in_docker(&[
            "lab-ops natmap daemon --socket /tmp/ns --state-dir /tmp/st --socket-group root &",
            "sleep 2",
            "&&",
            "lab-ops natmap --socket /tmp/ns hairpin --ext-ip 1.2.3.4 --int-ip 10.0.0.1 --ports 8080",
            "&&",
            "iptables-save",
            "|",
            "grep DNAT",
        ]);
        assert!(
            out.contains("--to-destination 10.0.0.1"),
            "Hairpin rule missing:\n{out}"
        );
    }
}
