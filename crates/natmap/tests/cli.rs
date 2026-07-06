use lab_ops_natmap::command::parse_docker_mapping;
use lab_ops_natmap::models::TransportProtocol;
use proptest::prelude::*;

fn arb_ipv4() -> impl Strategy<Value = String> {
    (any::<u8>(), any::<u8>(), any::<u8>(), any::<u8>())
        .prop_map(|(a, b, c, d)| format!("{a}.{b}.{c}.{d}"))
}

proptest! {
    #[test]
    fn parse_1_part_roundtrip(port in 1u16..=65535) {
        let req = parse_docker_mapping(&port.to_string()).unwrap();
        prop_assert_eq!(req.host_ip, "0.0.0.0");
        prop_assert_eq!(req.host_port, port);
        prop_assert_eq!(req.container_port, port);
        prop_assert_eq!(req.target_ip, None);
        prop_assert_eq!(req.proto, TransportProtocol::Tcp);
    }

    #[test]
    fn parse_2_part_ip_port(ip in arb_ipv4(), port in 1u16..=65535) {
        let mapping = format!("{ip}:{port}");
        let req = parse_docker_mapping(&mapping).unwrap();
        prop_assert_eq!(req.host_ip, ip);
        prop_assert_eq!(req.host_port, port);
        prop_assert_eq!(req.container_port, port);
        prop_assert_eq!(req.target_ip, None);
        prop_assert_eq!(req.proto, TransportProtocol::Tcp);
    }

    #[test]
    fn parse_2_part_port_port(host_port in 1u16..=65535, container_port in 1u16..=65535) {
        let mapping = format!("{host_port}:{container_port}");
        let req = parse_docker_mapping(&mapping).unwrap();
        prop_assert_eq!(req.host_ip, "0.0.0.0");
        prop_assert_eq!(req.host_port, host_port);
        prop_assert_eq!(req.container_port, container_port);
        prop_assert_eq!(req.target_ip, None);
    }

    #[test]
    fn parse_3_part_ip_port_port(ip in arb_ipv4(), host_port in 1u16..=65535, container_port in 1u16..=65535) {
        let mapping = format!("{ip}:{host_port}:{container_port}");
        let req = parse_docker_mapping(&mapping).unwrap();
        prop_assert_eq!(req.host_ip, ip);
        prop_assert_eq!(req.host_port, host_port);
        prop_assert_eq!(req.container_port, container_port);
        prop_assert_eq!(req.target_ip, None);
    }

    #[test]
    fn parse_3_part_port_ip_port(host_port in 1u16..=65535, target_ip in arb_ipv4(), container_port in 1u16..=65535) {
        let mapping = format!("{host_port}:{target_ip}:{container_port}");
        let req = parse_docker_mapping(&mapping).unwrap();
        prop_assert_eq!(req.host_ip, "0.0.0.0");
        prop_assert_eq!(req.host_port, host_port);
        prop_assert_eq!(req.container_port, container_port);
        prop_assert_eq!(req.target_ip.as_deref(), Some(target_ip.as_str()));
    }

    #[test]
    fn parse_4_part(host_ip in arb_ipv4(), host_port in 1u16..=65535, target_ip in arb_ipv4(), container_port in 1u16..=65535) {
        let mapping = format!("{host_ip}:{host_port}:{target_ip}:{container_port}");
        let req = parse_docker_mapping(&mapping).unwrap();
        prop_assert_eq!(req.host_ip, host_ip);
        prop_assert_eq!(req.host_port, host_port);
        prop_assert_eq!(req.container_port, container_port);
        prop_assert_eq!(req.target_ip.as_deref(), Some(target_ip.as_str()));
        prop_assert_eq!(req.proto, TransportProtocol::Tcp);
    }

    #[test]
    fn parse_with_proto(port in 1u16..=65535, proto in prop::sample::select(&["tcp", "udp"])) {
        let mapping = format!("{port}/{proto}");
        let req = parse_docker_mapping(&mapping).unwrap();
        prop_assert_eq!(req.host_port, port);
        prop_assert_eq!(req.container_port, port);
        let expected = if proto == "tcp" { TransportProtocol::Tcp } else { TransportProtocol::Udp };
        prop_assert_eq!(req.proto, expected);
    }

    #[test]
    fn parse_5_part_always_fails(a in arb_ipv4(), b in 1u16..=65535, c in arb_ipv4(), d in 1u16..=65535, e in 1u16..=65535) {
        let mapping = format!("{a}:{b}:{c}:{d}:{e}");
        prop_assert!(parse_docker_mapping(&mapping).is_err());
    }
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
fn parse_four_part_full() {
    let req = parse_docker_mapping("10.0.0.1:8080:192.168.1.5:80/tcp").unwrap();
    assert_eq!(req.host_ip, "10.0.0.1");
    assert_eq!(req.host_port, 8080);
    assert_eq!(req.container_port, 80);
    assert_eq!(req.target_ip.as_deref(), Some("192.168.1.5"));
    assert_eq!(req.proto, TransportProtocol::Tcp);
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

#[test]
fn parse_empty_string_returns_error() {
    assert!(parse_docker_mapping("").is_err());
}

#[test]
fn parse_colon_only_returns_error() {
    assert!(parse_docker_mapping(":").is_err());
}

#[test]
fn parse_double_colon_returns_error() {
    assert!(parse_docker_mapping("::").is_err());
}

#[test]
fn parse_port_zero_single() {
    let req = parse_docker_mapping("0").unwrap();
    assert_eq!(req.host_port, 0);
    assert_eq!(req.container_port, 0);
}

#[test]
fn parse_port_zero_with_protocol() {
    let req = parse_docker_mapping("0/udp").unwrap();
    assert_eq!(req.host_port, 0);
    assert_eq!(req.container_port, 0);
    assert_eq!(req.proto, TransportProtocol::Udp);
}

#[test]
fn parse_host_port_zero_container_port_nonzero() {
    let req = parse_docker_mapping("0:80").unwrap();
    assert_eq!(req.host_port, 0);
    assert_eq!(req.container_port, 80);
}

#[test]
fn parse_negative_port_returns_error() {
    assert!(parse_docker_mapping("-1").is_err());
}
