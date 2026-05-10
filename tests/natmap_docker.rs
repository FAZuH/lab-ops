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

    // Helper to run a command inside a docker container with lab-ops binary mounted
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
            panic!(
                "Docker command failed: {}\nstdout:\n{}\nstderr:\n{}",
                shell_cmd, stdout, stderr
            );
        }

        String::from_utf8_lossy(&output.stdout).to_string()
    }

    #[test]
    fn test_natmap_forward() {
        let ext_ip = "139.99.69.43";
        let int_ip = "10.10.10.101";
        let ports = "25,465";
        let proto = "tcp";

        // Add rules
        let save_out = run_in_docker(&[
            "lab-ops",
            "natmap",
            "forward",
            "--ext-ip",
            ext_ip,
            "--int-ip",
            int_ip,
            "--ports",
            ports,
            "&&",
            "iptables-save",
        ]);

        // Verify rules added
        assert!(
            save_out.contains(&format!(
                "-A PREROUTING -d {}/32 -p {} -m multiport --dports {} -j DNAT --to-destination {}",
                ext_ip, proto, ports, int_ip
            )),
            "PREROUTING rule missing or mismatched"
        );
        assert!(
            save_out.contains(&format!(
                "-A FORWARD -d {}/32 -p {} -m multiport --dports {} -j ACCEPT",
                int_ip, proto, ports
            )),
            "FORWARD rule missing or mismatched"
        );

        // Delete rules
        let save_out_after = run_in_docker(&[
            "lab-ops",
            "natmap",
            "forward",
            "--ext-ip",
            ext_ip,
            "--int-ip",
            int_ip,
            "--ports",
            ports,
            "&&",
            "lab-ops",
            "natmap",
            "forward",
            "--ext-ip",
            ext_ip,
            "--int-ip",
            int_ip,
            "--ports",
            ports,
            "--delete",
            "&&",
            "iptables-save",
        ]);

        // Verify rules deleted
        assert!(!save_out_after.contains(&format!(
            "-A PREROUTING -d {}/32 -p {} -m multiport --dports {} -j DNAT --to-destination {}",
            ext_ip, proto, ports, int_ip
        )));
        assert!(!save_out_after.contains(&format!(
            "-A FORWARD -d {}/32 -p {} -m multiport --dports {} -j ACCEPT",
            int_ip, proto, ports
        )));
    }

    #[test]
    fn test_natmap_snat() {
        let ext_ip = "139.99.69.43";
        let int_ip = "10.10.10.101";
        let ext_if = "vmbr0";

        let save_out = run_in_docker(&[
            "lab-ops",
            "natmap",
            "snat",
            "--ext-ip",
            ext_ip,
            "--int-ip",
            int_ip,
            "--ext-if",
            ext_if,
            "&&",
            "iptables-save",
        ]);

        assert!(
            save_out.contains(&format!(
                "-A POSTROUTING -s {}/32 -o {} -j SNAT --to-source {}",
                int_ip, ext_if, ext_ip
            )),
            "POSTROUTING SNAT rule missing or mismatched"
        );

        let save_out_after = run_in_docker(&[
            "lab-ops",
            "natmap",
            "snat",
            "--ext-ip",
            ext_ip,
            "--int-ip",
            int_ip,
            "--ext-if",
            ext_if,
            "&&",
            "lab-ops",
            "natmap",
            "snat",
            "--ext-ip",
            ext_ip,
            "--int-ip",
            int_ip,
            "--ext-if",
            ext_if,
            "--delete",
            "&&",
            "iptables-save",
        ]);

        assert!(!save_out_after.contains(&format!(
            "-A POSTROUTING -s {}/32 -o {} -j SNAT --to-source {}",
            int_ip, ext_if, ext_ip
        )));
    }

    #[test]
    fn test_natmap_hairpin() {
        let ext_ip = "139.99.69.43";
        let int_ip = "10.10.10.101";
        let ports = "25,465";
        let proto = "tcp";

        let save_out = run_in_docker(&[
            "lab-ops",
            "natmap",
            "hairpin",
            "--ext-ip",
            ext_ip,
            "--int-ip",
            int_ip,
            "--ports",
            ports,
            "&&",
            "iptables-save",
        ]);

        assert!(save_out.contains(&format!("-A PREROUTING -s {}/32 -d {}/32 -p {} -m multiport --dports {} -j DNAT --to-destination {}", int_ip, ext_ip, proto, ports, int_ip)));
        assert!(save_out.contains(&format!(
            "-A POSTROUTING -s {}/32 -d {}/32 -p {} -m multiport --dports {} -j MASQUERADE",
            int_ip, int_ip, proto, ports
        )));

        let save_out_after = run_in_docker(&[
            "lab-ops",
            "natmap",
            "hairpin",
            "--ext-ip",
            ext_ip,
            "--int-ip",
            int_ip,
            "--ports",
            ports,
            "&&",
            "lab-ops",
            "natmap",
            "hairpin",
            "--ext-ip",
            ext_ip,
            "--int-ip",
            int_ip,
            "--ports",
            ports,
            "--delete",
            "&&",
            "iptables-save",
        ]);

        assert!(!save_out_after.contains(&format!("-A PREROUTING -s {}/32 -d {}/32 -p {} -m multiport --dports {} -j DNAT --to-destination {}", int_ip, ext_ip, proto, ports, int_ip)));
        assert!(!save_out_after.contains(&format!(
            "-A POSTROUTING -s {}/32 -d {}/32 -p {} -m multiport --dports {} -j MASQUERADE",
            int_ip, int_ip, proto, ports
        )));
    }
}
