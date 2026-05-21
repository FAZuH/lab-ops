use natmap::models::DockerAddMapRequest;
use natmap::models::TransportProtocol;

/// Helper to parse a mapping string using the same logic as command.rs::add().
fn parse_mapping(mapping: &str) -> Result<DockerAddMapRequest, String> {
    let (mapping_part, proto) = match mapping.split_once('/') {
        Some((m, p)) => (m, p.to_string()),
        None => (mapping, "tcp".to_string()),
    };

    let parts: Vec<&str> = mapping_part.split(':').collect();

    let mut host_ip = "0.0.0.0".to_string();
    let mut host_port = 0u16;
    let mut target_ip = None;
    let mut container_port = 0u16;

    match parts.len() {
        1 => {
            host_port = parts[0]
                .parse()
                .map_err(|e: std::num::ParseIntError| e.to_string())?;
            container_port = host_port;
        }
        2 => {
            if let Ok(ip) = parts[0].parse::<std::net::IpAddr>() {
                host_ip = ip.to_string();
                host_port = parts[1]
                    .parse()
                    .map_err(|e: std::num::ParseIntError| e.to_string())?;
                container_port = host_port;
            } else {
                host_port = parts[0]
                    .parse()
                    .map_err(|e: std::num::ParseIntError| e.to_string())?;
                container_port = parts[1]
                    .parse()
                    .map_err(|e: std::num::ParseIntError| e.to_string())?;
            }
        }
        3 => {
            if let Ok(ip) = parts[0].parse::<std::net::IpAddr>() {
                host_ip = ip.to_string();
                host_port = parts[1]
                    .parse()
                    .map_err(|e: std::num::ParseIntError| e.to_string())?;
                container_port = parts[2]
                    .parse()
                    .map_err(|e: std::num::ParseIntError| e.to_string())?;
            } else {
                host_port = parts[0]
                    .parse()
                    .map_err(|e: std::num::ParseIntError| e.to_string())?;
                target_ip = Some(parts[1].to_string());
                container_port = parts[2]
                    .parse()
                    .map_err(|e: std::num::ParseIntError| e.to_string())?;
            }
        }
        4 => {
            host_ip = parts[0].to_string();
            host_port = parts[1]
                .parse()
                .map_err(|e: std::num::ParseIntError| e.to_string())?;
            target_ip = Some(parts[2].to_string());
            container_port = parts[3]
                .parse()
                .map_err(|e: std::num::ParseIntError| e.to_string())?;
        }
        _ => return Err("Invalid mapping format".into()),
    }

    let proto_enum = match proto.as_str() {
        "tcp" => TransportProtocol::Tcp,
        "udp" => TransportProtocol::Udp,
        _ => return Err("Invalid protocol".into()),
    };

    Ok(DockerAddMapRequest {
        host_ip,
        host_port,
        container_port,
        target_ip,
        proto: proto_enum,
        ..Default::default()
    })
}

#[test]
fn parse_two_part_host_port_container_port() {
    let req = parse_mapping("8080:80").unwrap();
    assert_eq!(req.host_ip, "0.0.0.0");
    assert_eq!(req.host_port, 8080);
    assert_eq!(req.container_port, 80);
    assert_eq!(req.target_ip, None);
    assert_eq!(req.proto, TransportProtocol::Tcp);
}

#[test]
fn parse_two_part_with_proto() {
    let req = parse_mapping("8080:80/udp").unwrap();
    assert_eq!(req.host_ip, "0.0.0.0");
    assert_eq!(req.host_port, 8080);
    assert_eq!(req.container_port, 80);
    assert_eq!(req.proto, TransportProtocol::Udp);
}

#[test]
fn parse_three_part_host_ip_host_port_container_port() {
    let req = parse_mapping("100.64.0.10:80:80").unwrap();
    assert_eq!(req.host_ip, "100.64.0.10");
    assert_eq!(req.host_port, 80);
    assert_eq!(req.container_port, 80);
    assert_eq!(req.target_ip, None);
    assert_eq!(req.proto, TransportProtocol::Tcp);
}

#[test]
fn parse_three_part_host_port_target_ip_container_port() {
    let req = parse_mapping("8080:127.0.0.1:80").unwrap();
    assert_eq!(req.host_ip, "0.0.0.0");
    assert_eq!(req.host_port, 8080);
    assert_eq!(req.container_port, 80);
    assert_eq!(req.target_ip.as_deref(), Some("127.0.0.1"));
    assert_eq!(req.proto, TransportProtocol::Tcp);
}

#[test]
fn parse_four_part_full() {
    let req = parse_mapping("10.0.0.1:8080:192.168.1.5:80/tcp").unwrap();
    assert_eq!(req.host_ip, "10.0.0.1");
    assert_eq!(req.host_port, 8080);
    assert_eq!(req.container_port, 80);
    assert_eq!(req.target_ip.as_deref(), Some("192.168.1.5"));
    assert_eq!(req.proto, TransportProtocol::Tcp);
}

#[test]
fn parse_single_port_only() {
    let req = parse_mapping("8080").unwrap();
    assert_eq!(req.host_ip, "0.0.0.0");
    assert_eq!(req.host_port, 8080);
    assert_eq!(req.container_port, 8080);
    assert_eq!(req.target_ip, None);
    assert_eq!(req.proto, TransportProtocol::Tcp);
}

#[test]
fn parse_host_ip_port_only() {
    let req = parse_mapping("100.64.0.10:8080").unwrap();
    assert_eq!(req.host_ip, "100.64.0.10");
    assert_eq!(req.host_port, 8080);
    assert_eq!(req.container_port, 8080);
    assert_eq!(req.target_ip, None);
}

#[test]
fn parse_three_part_ipv4_address() {
    let req = parse_mapping("192.168.1.100:9090:9090/tcp").unwrap();
    assert_eq!(req.host_ip, "192.168.1.100");
    assert_eq!(req.host_port, 9090);
    assert_eq!(req.container_port, 9090);
    assert_eq!(req.proto, TransportProtocol::Tcp);
}

#[test]
fn parse_ipv4_loopback_address() {
    let req = parse_mapping("127.0.0.1:3000:3000").unwrap();
    assert_eq!(req.host_ip, "127.0.0.1");
    assert_eq!(req.host_port, 3000);
    assert_eq!(req.container_port, 3000);
    assert_eq!(req.proto, TransportProtocol::Tcp);
}

#[test]
fn parse_invalid_five_parts() {
    let err = parse_mapping("a:b:c:d:e").unwrap_err();
    assert!(err.contains("Invalid mapping format"));
}

#[test]
fn parse_invalid_port() {
    let err = parse_mapping("8080:abc").unwrap_err();
    assert!(err.contains("invalid digit"));
}
