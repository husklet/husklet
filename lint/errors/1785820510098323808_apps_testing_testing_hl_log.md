# `testing -> hl-log`

- [ ] Approved
- Timestamp: `1785820510098323808`
- Domain: `apps`
- Package: `testing`
- Rule: `dependency-direction`
- Severity: `error`
- Source: `src/apps/testing/Cargo.toml:17:1`
- Queue: `unclassified`
- Source layer: `application`
- Target layer: `packages`
- Dependency kind: `normal`
- Cargo alias: `hl-log`
- Target condition: `all`

## Finding

the local dependency is not present in the checked engine package graph; `testing` has a normal dependency on `hl-log`

Help: remove the edge, invert it through a consumer-owned port, or update the reviewed package graph before adding the dependency

## Review

- Which domain owns the capability, and can the dependency be inverted through a narrow port?

## Decision


## Dependencies

- `hl-log`

## Source

````rust
hl-log = { path = "../../packages/hl-log" }
````

## Related context

### dependency target in packages layer

`src/packages/hl-log/Cargo.toml:8:1`

````rust
name = "hl-log"
````
