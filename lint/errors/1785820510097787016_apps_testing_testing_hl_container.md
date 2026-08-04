# `testing -> hl-container`

- [ ] Approved
- Timestamp: `1785820510097787016`
- Domain: `apps`
- Package: `testing`
- Rule: `dependency-direction`
- Severity: `error`
- Source: `src/apps/testing/Cargo.toml:15:1`
- Queue: `unclassified`
- Source layer: `application`
- Target layer: `containers`
- Dependency kind: `normal`
- Cargo alias: `hl-container`
- Target condition: `all`

## Finding

the local dependency is not present in the checked engine package graph; `testing` has a normal dependency on `hl-container`

Help: remove the edge, invert it through a consumer-owned port, or update the reviewed package graph before adding the dependency

## Review

- Which domain owns the capability, and can the dependency be inverted through a narrow port?

## Decision


## Dependencies

- `hl-container`

## Source

````rust
hl-container = { path = "../../containers/hl-container" }
````

## Related context

### dependency target in containers layer

`src/containers/hl-container/Cargo.toml:2:1`

````rust
name = "hl-container"
````
