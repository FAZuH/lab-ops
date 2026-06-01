use lab_ops_natmap::models::DockerAddMapRequest;
use lab_ops_natmap::models::TransportProtocol;

/// Helper to parse a mapping string using the same logic as command.rs::add().
fn parse_mapping(mapping: &str) -> Result<DockerAddMapRequest, String> {
    let (mapping_part, proto) = mapping
        .split_once('/')
        .map(|(m, p)| (m, p.to_string()))
        .unwrap_or((mapping, "tcp".to_string()));

    let parts: Vec<&str> = mapping_part.split(':').collect();

    let (host_ip, host_port, target_ip, container_port) = match parts.as_slice() {
        [port] => {
            let p = parse_port(port)?;
            ("0.0.0.0".to_string(), p, None, p)
        }
        [maybe_ip, port] => {
            if let Ok(ip) = maybe_ip.parse::<std::net::IpAddr>() {
                let p = parse_port(port)?;
                (ip.to_string(), p, None, p)
            } else {
                (
                    "0.0.0.0".to_string(),
                    parse_port(maybe_ip)?,
                    None,
                    parse_port(port)?,
                )
            }
        }
        [maybe_ip, host, container] => {
            if let Ok(ip) = maybe_ip.parse::<std::net::IpAddr>() {
                (
                    ip.to_string(),
                    parse_port(host)?,
                    None,
                    parse_port(container)?,
                )
            } else {
                (
                    "0.0.0.0".to_string(),
                    parse_port(maybe_ip)?,
                    Some(host.to_string()),
                    parse_port(container)?,
                )
            }
        }
        [host_ip, host, target, container] => (
            host_ip.to_string(),
            parse_port(host)?,
            Some(target.to_string()),
            parse_port(container)?,
        ),
        _ => return Err("Invalid mapping format".into()),
    };

    Ok(DockerAddMapRequest {
        host_ip,
        host_port,
        container_port,
        target_ip,
        proto: proto.parse().unwrap(),
    })
}

fn parse_port(s: &str) -> Result<u16, String> {
    s.parse()
        .map_err(|e: std::num::ParseIntError| e.to_string())
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
