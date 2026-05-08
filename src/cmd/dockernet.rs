use std::collections::HashMap;

use bollard;
use bollard::Docker;
use bollard::plugin::EndpointSettings;
use bollard::plugin::PortBinding;
use bollard::query_parameters::ListContainersOptionsBuilder;
use color_eyre::eyre::Result;
use comfy_table::Table;

pub async fn run() -> Result<()> {
    let mut table = Table::new();
    let mut rows = Vec::new();
    table.set_header(vec!["Name", "Status", "IP", "Binds"]);

    let docker = Docker::connect_with_local_defaults()?;
    let opt = Some(ListContainersOptionsBuilder::new().all(true).build());

    for cont in docker.list_containers(opt).await? {
        let Some(id) = cont.id else { continue };
        let status = cont.status.unwrap_or_default();
        if is_exit(&status) {
            continue;
        }

        let name = format_name(cont.names);

        let Some(network) = docker.inspect_container(&id, None).await?.network_settings else {
            continue;
        };

        let ips = format_ips(network.networks);
        let binds = format_binds(network.ports);

        rows.push([name, status, ips, binds]);
    }

    rows.sort_by_key(|r| r[0].clone());
    rows.sort_by_key(|r| r[1].clone());
    rows.sort_by_key(|r| r[3].clone());

    table.add_rows(rows);

    println!("{table}");
    Ok(())
}

fn is_exit(status: impl AsRef<str>) -> bool {
    status.as_ref().contains("Exited (0)")
}

fn format_name(names: Option<Vec<String>>) -> String {
    names
        .unwrap_or_default()
        .into_iter()
        .next()
        .unwrap_or_default()
}

fn format_ips(networks: Option<HashMap<String, EndpointSettings>>) -> String {
    let Some(networks) = networks else {
        return String::new();
    };
    networks
        .values()
        .filter_map(|nw| nw.ip_address.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_binds(ports: Option<HashMap<String, Option<Vec<PortBinding>>>>) -> String {
    let Some(ports) = ports else {
        return String::new();
    };
    ports
        .iter()
        .filter_map(|(port, binds)| {
            let binds = binds.as_ref()?;
            let proto = port.split('/').nth(1).unwrap_or("");
            let addrs = binds
                .iter()
                .filter_map(|b| {
                    let ip = b.host_ip.as_ref()?;
                    let port = b.host_port.as_deref().unwrap_or("");
                    Some(format!("{ip}:{port}"))
                })
                .collect::<Vec<_>>()
                .join(", ");
            Some(format!("{proto}: {addrs}"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use bollard::models::EndpointSettings;
    use bollard::models::PortBinding;

    use super::*;

    fn make_networks(ips: &[&str]) -> Option<HashMap<String, EndpointSettings>> {
        Some(
            ips.iter()
                .enumerate()
                .map(|(i, ip)| {
                    (
                        format!("net{i}"),
                        EndpointSettings {
                            ip_address: Some(ip.to_string()),
                            ..Default::default()
                        },
                    )
                })
                .collect(),
        )
    }

    fn make_ports(
        bindings: &[(&str, &[(&str, &str)])],
    ) -> Option<HashMap<String, Option<Vec<PortBinding>>>> {
        Some(
            bindings
                .iter()
                .map(|(port, binds)| {
                    let binds = binds
                        .iter()
                        .map(|(ip, p)| PortBinding {
                            host_ip: Some(ip.to_string()),
                            host_port: Some(p.to_string()),
                        })
                        .collect();
                    (port.to_string(), Some(binds))
                })
                .collect(),
        )
    }

    #[test]
    fn test_format_ips_none() {
        assert_eq!(format_ips(None), "");
    }

    #[test]
    fn test_format_ips_single() {
        assert_eq!(format_ips(make_networks(&["172.19.0.2"])), "172.19.0.2");
    }

    #[test]
    fn test_format_ips_multiple() {
        let result = format_ips(make_networks(&["172.19.0.2", "10.0.0.1"]));
        let mut lines: Vec<&str> = result.lines().collect();
        lines.sort();
        assert_eq!(lines, vec!["10.0.0.1", "172.19.0.2"]);
    }

    #[test]
    fn test_format_binds_none() {
        assert_eq!(format_binds(None), "");
    }

    #[test]
    fn test_format_binds_single() {
        let ports = make_ports(&[("5432/tcp", &[("0.0.0.0", "5432")])]);
        assert_eq!(format_binds(ports), "tcp: 0.0.0.0:5432");
    }

    #[test]
    fn test_format_binds_multiple_ips() {
        let ports = make_ports(&[("5432/tcp", &[("0.0.0.0", "5432"), ("::", "5432")])]);
        assert_eq!(format_binds(ports), "tcp: 0.0.0.0:5432, :::5432");
    }

    #[test]
    fn test_format_binds_no_proto() {
        let ports = make_ports(&[("5432", &[("0.0.0.0", "5432")])]);
        assert_eq!(format_binds(ports), ": 0.0.0.0:5432");
    }

    #[test]
    fn test_format_binds_null_binding() {
        let mut map = HashMap::new();
        map.insert("5432/tcp".to_string(), None);
        assert_eq!(format_binds(Some(map)), "");
    }
}
