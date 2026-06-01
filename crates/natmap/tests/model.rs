use std::net::IpAddr;
use std::net::SocketAddr;
use std::str::FromStr;

use lab_ops_natmap::models::DockerAddMapRequest;
use lab_ops_natmap::models::DockerPortMap;
use lab_ops_natmap::models::DockerPortMapRequest;
use lab_ops_natmap::models::TransportProtocol;

#[test]
fn add_mapping_request_defaults() {
    let json = r#"{"host_port": 8080, "container_port": 80}"#;
    let req: DockerAddMapRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.host_ip, "0.0.0.0");
    assert_eq!(req.host_port, 8080);
    assert_eq!(req.container_port, 80);
    assert_eq!(req.proto, TransportProtocol::Tcp);
}

#[test]
fn add_mapping_request_full_fields() {
    let json =
        r#"{"host_ip": "127.0.0.1", "host_port": 3000, "container_port": 3000, "proto": "udp"}"#;
    let req: DockerAddMapRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.host_ip, "127.0.0.1");
    assert_eq!(req.host_port, 3000);
    assert_eq!(req.container_port, 3000);
    assert_eq!(req.proto, TransportProtocol::Udp);
    assert_eq!(req.target_ip, None);
}

#[test]
fn add_mapping_request_with_target_ip() {
    let json = r#"{"host_ip": "0.0.0.0", "host_port": 8080, "container_port": 80, "target_ip": "127.0.0.1"}"#;
    let req: DockerAddMapRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.host_ip, "0.0.0.0");
    assert_eq!(req.target_ip.as_deref(), Some("127.0.0.1"));
    assert_eq!(req.container_port, 80);
}

#[test]
fn add_mapping_request_serialize_defaults() {
    let req = DockerAddMapRequest {
        host_ip: "0.0.0.0".into(),
        host_port: 8080,
        container_port: 80,
        proto: TransportProtocol::Tcp,
        ..Default::default()
    };
    let json = serde_json::to_string(&req).unwrap();
    assert!(json.contains("\"host_ip\":\"0.0.0.0\""));
    assert!(json.contains("\"proto\":\"tcp\""));
}

#[test]
fn transport_protocol_display() {
    assert_eq!(TransportProtocol::Tcp.to_string(), "tcp");
    assert_eq!(TransportProtocol::Udp.to_string(), "udp");
}

#[test]
fn port_mapping_request_is_ipv6() {
    let ipv4 = DockerPortMapRequest {
        host_addr: SocketAddr::new(IpAddr::from_str("0.0.0.0").unwrap(), 80),
        container_addr: SocketAddr::new(IpAddr::from_str("172.17.0.2").unwrap(), 80),
        proto: TransportProtocol::Tcp,
    };
    assert!(!ipv4.is_ipv6());

    let ipv6 = DockerPortMapRequest {
        host_addr: SocketAddr::new(IpAddr::from_str("::").unwrap(), 80),
        container_addr: SocketAddr::new(IpAddr::from_str("172.17.0.2").unwrap(), 80),
        proto: TransportProtocol::Tcp,
    };
    assert!(ipv6.is_ipv6());
}

#[test]
fn active_port_mapping_rule_comment_format() {
    let req = DockerPortMapRequest {
        host_addr: SocketAddr::new(IpAddr::from_str("0.0.0.0").unwrap(), 8080),
        container_addr: SocketAddr::new(IpAddr::from_str("172.17.0.2").unwrap(), 80),
        proto: TransportProtocol::Tcp,
    };
    let mapping = DockerPortMap::new(1, req, "abc123".into(), "my-nginx".into());
    assert_eq!(mapping.rule_comment, "natmap:abc123:8080");
    assert_eq!(mapping.container_id, "abc123");
    assert_eq!(mapping.container_name, "my-nginx");
}

#[test]
fn output_dnat_always_added() {
    // OUTPUT DNAT is now always installed — the destination IP varies
    let host_ips = [
        "0.0.0.0",
        "::",
        "127.0.0.1",
        "::1",
        "100.64.0.10",
        "192.168.1.1",
    ];
    for ip_str in host_ips {
        let ip = IpAddr::from_str(ip_str).unwrap();
        let dst = output_dnat_destination(ip);
        assert!(!dst.is_empty(), "should have a destination for {ip_str}");
    }
}

#[test]
fn output_dnat_destination_for_unspecified() {
    assert_eq!(
        output_dnat_destination(IpAddr::from_str("0.0.0.0").unwrap()),
        "127.0.0.1"
    );
    assert_eq!(
        output_dnat_destination(IpAddr::from_str("::").unwrap()),
        "::1"
    );
}

#[test]
fn output_dnat_destination_for_specific_ip() {
    assert_eq!(
        output_dnat_destination(IpAddr::from_str("100.64.0.10").unwrap()),
        "100.64.0.10"
    );
    assert_eq!(
        output_dnat_destination(IpAddr::from_str("192.168.1.1").unwrap()),
        "192.168.1.1"
    );
    assert_eq!(
        output_dnat_destination(IpAddr::from_str("127.0.0.1").unwrap()),
        "127.0.0.1"
    );
}

/// Mirrors the OUTPUT DNAT destination logic in install_mapping
fn output_dnat_destination(host_ip: IpAddr) -> String {
    if host_ip.is_unspecified() {
        if host_ip.is_ipv6() {
            "::1".to_string()
        } else {
            "127.0.0.1".to_string()
        }
    } else {
        host_ip.to_string()
    }
}
