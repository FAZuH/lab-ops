use lab_ops_natmap::command::parse_docker_mapping;
use lab_ops_natmap::models::TransportProtocol;

#[test]
fn parse_two_part_host_port_container_port() {
    let req = parse_docker_mapping("8080:80").unwrap();
    assert_eq!(req.host_ip, "0.0.0.0");
    assert_eq!(req.host_port, 8080);
    assert_eq!(req.container_port, 80);
    assert_eq!(req.target_ip, None);
    assert_eq!(req.proto, TransportProtocol::Tcp);
}

#[test]
fn parse_two_part_with_proto() {
    let req = parse_docker_mapping("8080:80/udp").unwrap();
    assert_eq!(req.host_ip, "0.0.0.0");
    assert_eq!(req.host_port, 8080);
    assert_eq!(req.container_port, 80);
    assert_eq!(req.proto, TransportProtocol::Udp);
}

#[test]
fn parse_three_part_host_ip_host_port_container_port() {
    let req = parse_docker_mapping("100.64.0.10:80:80").unwrap();
    assert_eq!(req.host_ip, "100.64.0.10");
    assert_eq!(req.host_port, 80);
    assert_eq!(req.container_port, 80);
    assert_eq!(req.target_ip, None);
    assert_eq!(req.proto, TransportProtocol::Tcp);
}

#[test]
fn parse_three_part_host_port_target_ip_container_port() {
    let req = parse_docker_mapping("8080:127.0.0.1:80").unwrap();
    assert_eq!(req.host_ip, "0.0.0.0");
    assert_eq!(req.host_port, 8080);
    assert_eq!(req.container_port, 80);
    assert_eq!(req.target_ip.as_deref(), Some("127.0.0.1"));
    assert_eq!(req.proto, TransportProtocol::Tcp);
}

#[test]
fn parse_four_part_full() {
    let req = parse_docker_mapping("10.0.0.1:8080:192.168.1.5:80/tcp").unwrap();
    assert_eq!(req.host_ip, "10.0.0.1");
    assert_eq!(req.host_port, 8080);
    assert_eq!(req.container_port, 80);
    assert_eq!(req.target_ip.as_deref(), Some("192.168.1.5"));
    assert_eq!(req.proto, TransportProtocol::Tcp);
}

#[test]
fn parse_single_port_only() {
    let req = parse_docker_mapping("8080").unwrap();
    assert_eq!(req.host_ip, "0.0.0.0");
    assert_eq!(req.host_port, 8080);
    assert_eq!(req.container_port, 8080);
    assert_eq!(req.target_ip, None);
    assert_eq!(req.proto, TransportProtocol::Tcp);
}

#[test]
fn parse_host_ip_port_only() {
    let req = parse_docker_mapping("100.64.0.10:8080").unwrap();
    assert_eq!(req.host_ip, "100.64.0.10");
    assert_eq!(req.host_port, 8080);
    assert_eq!(req.container_port, 8080);
    assert_eq!(req.target_ip, None);
}

#[test]
fn parse_three_part_ipv4_address() {
    let req = parse_docker_mapping("192.168.1.100:9090:9090/tcp").unwrap();
    assert_eq!(req.host_ip, "192.168.1.100");
    assert_eq!(req.host_port, 9090);
    assert_eq!(req.container_port, 9090);
    assert_eq!(req.proto, TransportProtocol::Tcp);
}

#[test]
fn parse_ipv4_loopback_address() {
    let req = parse_docker_mapping("127.0.0.1:3000:3000").unwrap();
    assert_eq!(req.host_ip, "127.0.0.1");
    assert_eq!(req.host_port, 3000);
    assert_eq!(req.container_port, 3000);
    assert_eq!(req.proto, TransportProtocol::Tcp);
}

#[test]
fn parse_invalid_five_parts() {
    let err = parse_docker_mapping("a:b:c:d:e").unwrap_err();
    assert!(err.to_string().contains("Invalid mapping format"));
}

#[test]
fn parse_invalid_port() {
    let err = parse_docker_mapping("8080:abc").unwrap_err();
    assert!(err.to_string().contains("invalid digit"));
}

#[test]
fn parse_invalid_protocol() {
    let err = parse_docker_mapping("8080/xyz").unwrap_err();
    assert!(err.to_string().contains("Invalid transport protocol"));
}
