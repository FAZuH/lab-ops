use dockernatmap::models::AddMappingRequest;

/// Simulates the CLI mapping string parsing logic
fn parse_mapping(mapping: &str) -> Result<AddMappingRequest, String> {
    let (mapping_part, proto) = match mapping.split_once('/') {
        Some((m, p)) => (m, p.to_string()),
        None => (mapping, "tcp".to_string()),
    };

    let parts: Vec<&str> = mapping_part.split(':').collect();
    let (host_ip, host_port, container_port) = match parts.len() {
        3 => (
            parts[0].to_string(),
            parts[1].parse::<u16>().map_err(|e| e.to_string())?,
            parts[2].parse::<u16>().map_err(|e| e.to_string())?,
        ),
        2 => (
            "0.0.0.0".to_string(),
            parts[0].parse::<u16>().map_err(|e| e.to_string())?,
            parts[1].parse::<u16>().map_err(|e| e.to_string())?,
        ),
        _ => {
            return Err(
                "Invalid mapping format. Use [HOST_IP:]HOST_PORT:CONTAINER_PORT[/PROTO]".into(),
            );
        }
    };

    Ok(AddMappingRequest {
        host_ip,
        host_port,
        container_port,
        proto,
    })
}

#[test]
fn parse_simple_two_part() {
    let req = parse_mapping("8080:80").unwrap();
    assert_eq!(req.host_ip, "0.0.0.0");
    assert_eq!(req.host_port, 8080);
    assert_eq!(req.container_port, 80);
    assert_eq!(req.proto, "tcp");
}

#[test]
fn parse_two_part_with_proto() {
    let req = parse_mapping("8080:80/udp").unwrap();
    assert_eq!(req.host_ip, "0.0.0.0");
    assert_eq!(req.host_port, 8080);
    assert_eq!(req.container_port, 80);
    assert_eq!(req.proto, "udp");
}

#[test]
fn parse_three_part_with_ip() {
    let req = parse_mapping("100.64.0.10:80:80").unwrap();
    assert_eq!(req.host_ip, "100.64.0.10");
    assert_eq!(req.host_port, 80);
    assert_eq!(req.container_port, 80);
    assert_eq!(req.proto, "tcp");
}

#[test]
fn parse_three_part_with_ip_and_proto() {
    let req = parse_mapping("127.0.0.1:8443:443/tcp").unwrap();
    assert_eq!(req.host_ip, "127.0.0.1");
    assert_eq!(req.host_port, 8443);
    assert_eq!(req.container_port, 443);
    assert_eq!(req.proto, "tcp");
}

#[test]
fn parse_ipv4_loopback_address() {
    let req = parse_mapping("127.0.0.1:3000:3000").unwrap();
    assert_eq!(req.host_ip, "127.0.0.1");
    assert_eq!(req.host_port, 3000);
    assert_eq!(req.container_port, 3000);
    assert_eq!(req.proto, "tcp");
}

#[test]
fn parse_three_part_ipv4_address() {
    let req = parse_mapping("192.168.1.100:9090:9090/tcp").unwrap();
    assert_eq!(req.host_ip, "192.168.1.100");
    assert_eq!(req.host_port, 9090);
    assert_eq!(req.container_port, 9090);
    assert_eq!(req.proto, "tcp");
}

#[test]
fn parse_invalid_one_part() {
    let err = parse_mapping("8080").unwrap_err();
    assert!(err.contains("Invalid mapping format"));
}

#[test]
fn parse_invalid_four_parts() {
    let err = parse_mapping("a:b:c:d").unwrap_err();
    assert!(err.contains("Invalid mapping format"));
}
