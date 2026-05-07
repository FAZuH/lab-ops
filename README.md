# lab-ops

Personal utility tools for my homelab.

## Usage

```
lab-ops cf2ansible <zone-file> [zone-name]
```

Converts a BIND DNS zone file into Ansible Cloudflare DNS tasks (community.general.cloudflare_dns).

```
lab-ops cf2ansible example.com.txt
lab-ops cf2ansible example.com.txt example.com
```

## License

[MIT](https://spdx.org/licenses/MIT.html)
