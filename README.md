# lab-ops

Personal utility tools for my homelab.

## Usage

```bash
lab-ops <cmd> [args...]
```

### dockernet

Prints IP addresses and port bindings of Docker containers

```
lab-ops dockernet
```

### cf2ansble

```bash
lab-ops cf2ansible <zone-file> [zone-name]
```

Converts a BIND DNS zone file into Ansible Cloudflare DNS tasks (community.general.cloudflare_dns).

```bash
lab-ops cf2ansible example.com.txt
lab-ops cf2ansible example.com.txt example.com
```

## License

[MIT](https://spdx.org/licenses/MIT.html)
