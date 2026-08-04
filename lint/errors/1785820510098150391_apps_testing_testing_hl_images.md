# `testing -> hl-images`

- [ ] Approved
- Timestamp: `1785820510098150391`
- Domain: `apps`
- Package: `testing`
- Rule: `dependency-direction`
- Severity: `error`
- Source: `src/apps/testing/Cargo.toml:16:1`
- Queue: `unclassified`
- Source layer: `application`
- Target layer: `containers`
- Dependency kind: `normal`
- Cargo alias: `hl-images`
- Target condition: `all`

## Finding

the local dependency is not present in the checked engine package graph; `testing` has a normal dependency on `hl-images`

Help: remove the edge, invert it through a consumer-owned port, or update the reviewed package graph before adding the dependency

## Review

- Which domain owns the capability, and can the dependency be inverted through a narrow port?

## Decision


## Dependencies

- `hl-images`

## Source

````rust
hl-images = { path = "../../containers/hl-images" }
````

## Related context

### dependency target in containers layer

`src/containers/hl-images/Cargo.toml:2:1`

````rust
name = "hl-images"
````
