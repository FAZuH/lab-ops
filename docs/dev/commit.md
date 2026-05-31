Commits follow [Conventional Commits](https://www.conventionalcommits.org).

## Scopes

- `ad`: Changes to auto-discover workspace
- `nm`: Changes to natmap workspace
- `ll`: Changes to lab-lib workspace
- `cli`: CLI commands

## Changelog

Commits tagged with `[pub]` or `[public]` in the header, body, or subject are included in the auto-generated changelog. Commits without this marker are excluded (useful for internal refactors, chores, etc.).

Changelogs contain three sections:

| Section | Trigger |
|---------|---------|
| New Features | `feat(...): ... [pub]` |
| Bug Fixes | `fix(...): ... [pub]` |
| General | All other tagged public commits |

## Body Metadata

Additional metadata can be placed in the commit body:

```
scope: <section_name>      # Override the changelog section for this commit
changelog: <text>          # Override the commit subject in the changelog
```

The changelog generation is configured in [`.github/.config.js`](../../.github/.config.js), which uses `conventional-changelog-conventionalcommits` with custom transform logic for `[pub]` filtering and body metadata extraction.

### Bumping

Release versions follow semver. The bump level is determined by `chore!(major)` and `chore!(minor)` commit headers:

| Header | Bump |
|--------|------|
| `chore!(major)` | Major release |
| `chore!(minor)` | Minor release |
| (default) | Patch release |
