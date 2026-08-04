# `wire.rs`

- [ ] Approved
- Timestamp: `1785820518465901141`
- Domain: `containers`
- Package: `hl_daemon`
- Rule: `integration-test-candidate`
- Severity: `warning`
- Source: `src/containers/hl-daemon/src/api/http/network/wire.rs:224:1`
- Queue: `integration-placement-review`
- evidence: `public crate surface only`

## Finding

source unit tests use only the ordinary public crate API and are integration-test candidates

Help: review whether cross-domain/public behavior belongs under the crate tests/ boundary; syntax proves candidacy only when no private dependency is visible, and this rule never moves code automatically

## Review

- Does this test validate a public contract across domain boundaries?
- Would moving it lose useful private invariant coverage?

## Decision


## Dependencies

- None detected

## Source

````rust
#[cfg(test)]
mod tests {
    use crate::api::{EndpointConfig, NetworkCreate};
    use axum::http::StatusCode;

    #[test]
    fn network_create_preserves_and_rejects_meaningful_unknown_fields() {
        let harmless: NetworkCreate = serde_json::from_value(serde_json::json!({
            "Name": "isolated",
            "Driver": "none",
            "FutureOption": false
        }))
        .unwrap();
        harmless.spec().unwrap();

        let meaningful: NetworkCreate = serde_json::from_value(serde_json::json!({
            "Name": "isolated",
            "Driver": "none",
            "FutureOption": "enabled"
        }))
        .unwrap();
        let error = meaningful.spec().unwrap_err();
        assert_eq!(error.status, StatusCode::NOT_IMPLEMENTED);
        assert!(format!("{error:?}").contains("FutureOption"));

        let nested: NetworkCreate = serde_json::from_value(serde_json::json!({
            "Name": "bridge",
            "IPAM": {"Config": [], "FuturePoolPolicy": "strict"}
        }))
        .unwrap();
        assert!(format!("{:?}", nested.spec().unwrap_err()).contains("FuturePoolPolicy"));

        let endpoint: EndpointConfig = serde_json::from_value(serde_json::json!({
            "IPAMConfig": {"IPv4Address": "10.0.0.2", "FutureRoute": true}
        }))
        .unwrap();
        assert!(format!("{:?}", endpoint.spec().unwrap_err()).contains("FutureRoute"));
    }
}
````

## Related context

No related locations found in the scanned tree.
