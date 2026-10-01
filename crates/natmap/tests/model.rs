use std::net::IpAddr;
use std::net::SocketAddr;
use std::str::FromStr;

use lab_ops_natmap::models::DaemonState;
use lab_ops_natmap::models::DnatConfig;
use lab_ops_natmap::models::DockerAddMapRequest;
use lab_ops_natmap::models::DockerPortMap;
use lab_ops_natmap::models::DockerPortMapRequest;
use lab_ops_natmap::models::HairpinConfig;
use lab_ops_natmap::models::PolicyRouteConfig;
use lab_ops_natmap::models::SnatConfig;
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
    let value = serde_json::to_value(&req).unwrap();
    assert_eq!(value["host_ip"], "0.0.0.0");
    assert_eq!(value["proto"], "tcp");
    assert_eq!(value["target_ip"], serde_json::Value::Null);
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
fn port_mapping_request_rejects_unknown_proto() {
    let json =
        r#"{"host_addr": "0.0.0.0:8080", "container_addr": "172.17.0.2:80", "proto": "sctp"}"#;
    assert!(serde_json::from_str::<DockerPortMapRequest>(json).is_err());
}

#[test]
fn add_mapping_request_rejects_unknown_proto() {
    let json = r#"{"host_port": 8080, "container_port": 80, "proto": "sctp"}"#;
    assert!(serde_json::from_str::<DockerAddMapRequest>(json).is_err());
}

#[test]
fn dnat_config_rejects_unknown_proto() {
    let json =
        r#"{"ext_ip": "203.0.113.50", "int_ip": "10.0.0.99", "ports": "80", "proto": "icmp"}"#;
    assert!(serde_json::from_str::<DnatConfig>(json).is_err());
}

#[test]
fn hairpin_config_rejects_unknown_proto() {
    let json =
        r#"{"ext_ip": "203.0.113.50", "int_ip": "10.0.0.99", "ports": "443", "proto": "icmp"}"#;
    assert!(serde_json::from_str::<HairpinConfig>(json).is_err());
}

#[test]
fn dnat_config_rejects_missing_ext_ip() {
    let json = r#"{"int_ip": "10.0.0.99", "ports": "80", "proto": "tcp"}"#;
    let err = serde_json::from_str::<DnatConfig>(json)
        .unwrap_err()
        .to_string();
    assert!(err.contains("ext_ip"), "{err}");
}

#[test]
fn dnat_config_rejects_missing_int_ip() {
    let json = r#"{"ext_ip": "203.0.113.50", "ports": "80", "proto": "tcp"}"#;
    let err = serde_json::from_str::<DnatConfig>(json)
        .unwrap_err()
        .to_string();
    assert!(err.contains("int_ip"), "{err}");
}

#[test]
fn dnat_config_rejects_missing_ports() {
    let json = r#"{"ext_ip": "203.0.113.50", "int_ip": "10.0.0.99", "proto": "tcp"}"#;
    let err = serde_json::from_str::<DnatConfig>(json)
        .unwrap_err()
        .to_string();
    assert!(err.contains("ports"), "{err}");
}

#[test]
fn snat_config_rejects_missing_ext_if() {
    let json = r#"{"int_ip": "10.0.0.1", "ext_ip": "203.0.113.50"}"#;
    let err = serde_json::from_str::<SnatConfig>(json)
        .unwrap_err()
        .to_string();
    assert!(err.contains("ext_if"), "{err}");
}

#[test]
fn hairpin_config_rejects_missing_ext_ip() {
    let json = r#"{"int_ip": "10.0.0.99", "ports": "443", "proto": "udp"}"#;
    let err = serde_json::from_str::<HairpinConfig>(json)
        .unwrap_err()
        .to_string();
    assert!(err.contains("ext_ip"), "{err}");
}

#[test]
fn add_mapping_request_rejects_host_port_above_u16_max() {
    let json = r#"{"host_port": 65536, "container_port": 80}"#;
    assert!(serde_json::from_str::<DockerAddMapRequest>(json).is_err());
}

#[test]
fn add_mapping_request_rejects_container_port_above_u16_max() {
    let json = r#"{"host_port": 8080, "container_port": 65536}"#;
    assert!(serde_json::from_str::<DockerAddMapRequest>(json).is_err());
}

#[test]
fn add_mapping_request_accepts_u16_boundary_ports() {
    let json = r#"{"host_port": 65535, "container_port": 0}"#;
    let req: DockerAddMapRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.host_port, 65535);
    assert_eq!(req.container_port, 0);
}

#[test]
fn policy_route_config_rejects_table_above_u32_max() {
    let json = r#"{"src_ip": "10.10.10.10", "via": "192.168.1.1", "table": 4294967296}"#;
    assert!(serde_json::from_str::<PolicyRouteConfig>(json).is_err());
}

#[test]
fn daemon_state_loads_with_policy_routes() {
    let json = r#"{
        "mapping": {
            "c0ffee12": [{
                "id": 7,
                "request": {"host_addr": "0.0.0.0:8080", "container_addr": "172.17.0.2:80", "proto": "tcp"},
                "container_id": "c0ffee12",
                "container_name": "nginx-日本",
                "rule_comment": "natmap:c0ffee12:8080"
            }]
        },
        "dnats": [{"ext_ip": "203.0.113.50", "int_ip": "10.0.0.99", "ports": "80,443", "proto": "tcp", "ext_if": null, "preserve_src_ip": false}],
        "snats": [{"int_ip": "10.0.0.1", "ext_ip": "203.0.113.50", "ext_if": "eth0"}],
        "hairpins": [{"ext_ip": "203.0.113.50", "int_ip": "10.0.0.99", "ports": "80", "proto": "udp", "lan_cidr": null}],
        "policy_routes": [{"src_ip": "10.10.10.10", "via": "192.168.1.1", "table": 100}]
    }"#;
    let state: DaemonState = serde_json::from_str(json).unwrap();
    assert_eq!(state.mapping["c0ffee12"][0].container_name, "nginx-日本");
    assert_eq!(state.dnats[0].ports, "80,443");
    assert_eq!(state.snats[0].ext_if, "eth0");
    assert_eq!(state.hairpins[0].proto, TransportProtocol::Udp);
    assert_eq!(state.policy_routes[0].table, 100);
}

#[test]
fn daemon_state_loads_without_policy_routes() {
    let json = r#"{
        "mapping": {},
        "dnats": [{"ext_ip": "203.0.113.50", "int_ip": "10.0.0.99", "ports": "80", "proto": "tcp", "ext_if": null, "preserve_src_ip": false}],
        "snats": [],
        "hairpins": []
    }"#;
    let state: DaemonState = serde_json::from_str(json).unwrap();
    assert!(state.policy_routes.is_empty());
    assert_eq!(state.dnats.len(), 1);
}

#[test]
fn daemon_state_rejects_missing_required_key() {
    let json = r#"{
        "mapping": {},
        "snats": [],
        "hairpins": [],
        "policy_routes": []
    }"#;
    let err = serde_json::from_str::<DaemonState>(json)
        .unwrap_err()
        .to_string();
    assert!(err.contains("dnats"), "{err}");
}
