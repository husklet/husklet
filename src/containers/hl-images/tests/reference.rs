use hl_images::Reference;

#[test]
fn references_are_normalized_and_validated() {
    let alpine: Reference = "alpine".parse().unwrap();
    assert_eq!(alpine.registry(), "registry-1.docker.io");
    assert_eq!(alpine.repository(), "library/alpine");
    assert_eq!(alpine.tag(), Some("latest"));
    assert_eq!(alpine.to_string(), "docker.io/library/alpine:latest");

    let local: Reference = "localhost:5000/team/app:v1".parse().unwrap();
    assert_eq!(local.registry(), "localhost:5000");
    assert_eq!(local.repository(), "team/app");
    assert_eq!(local.tag(), Some("v1"));

    let digest = "a".repeat(64);
    let pinned: Reference = format!("ghcr.io/husklet/app@sha256:{digest}")
        .parse()
        .unwrap();
    assert_eq!(pinned.tag(), None);
    assert_eq!(pinned.manifest_selector(), format!("sha256:{digest}"));
}

#[test]
fn malformed_references_are_rejected() {
    for value in [
        "",
        "repo:",
        "repo@",
        "repo@sha256:bad",
        "host//repo",
        "../repo",
        "repo name",
        ":tag",
        "a:b:c",
    ] {
        assert!(value.parse::<Reference>().is_err(), "accepted {value:?}");
    }
}

#[test]
fn persisted_domain_values_cannot_bypass_validation() {
    assert!(serde_json::from_str::<Reference>(r#""../escape""#).is_err());
    assert!(serde_json::from_str::<hl_images::Digest>(r#""sha256:bad""#).is_err());
    let reference: Reference = "ghcr.io/husklet/app:v1".parse().unwrap();
    assert_eq!(
        serde_json::from_str::<Reference>(&serde_json::to_string(&reference).unwrap()).unwrap(),
        reference
    );
}

#[test]
fn hub_single_name_short_and_canonical() {
    let reference: Reference = "ubuntu".parse().unwrap();
    assert_eq!(reference.registry(), "registry-1.docker.io");
    assert_eq!(reference.repository(), "library/ubuntu");
    assert_eq!(reference.to_string(), "docker.io/library/ubuntu:latest");
}

#[test]
fn hub_user_repo_keeps_namespace() {
    let reference: Reference = "user/app:1".parse().unwrap();
    assert_eq!(reference.repository(), "user/app");
    assert_eq!(reference.to_string(), "docker.io/user/app:1");
}

#[test]
fn other_registry_shown_in_short() {
    let reference: Reference = "ghcr.io/o/a:v2".parse().unwrap();
    assert_eq!(reference.registry(), "ghcr.io");
    assert_eq!(reference.to_string(), "ghcr.io/o/a:v2");
}

#[test]
fn localhost_registry_with_port() {
    let reference: Reference = "localhost:5000/img".parse().unwrap();
    assert_eq!(reference.registry(), "localhost:5000");
    assert_eq!(reference.repository(), "img");
    assert_eq!(reference.tag(), Some("latest"));
}

#[test]
fn digest_pinned_reference_parses_repository_and_digest() {
    let digest = "a".repeat(64);
    let reference: Reference = format!("alpine@sha256:{digest}").parse().unwrap();
    assert_eq!(reference.repository(), "library/alpine");
    assert_eq!(reference.tag(), None);
    assert_eq!(reference.manifest_selector(), format!("sha256:{digest}"));
}

#[test]
fn digest_pinned_reference_with_registry_and_tag() {
    let digest = "b".repeat(64);
    let reference: Reference = format!("ghcr.io/o/a:v2@sha256:{digest}").parse().unwrap();
    assert_eq!(reference.registry(), "ghcr.io");
    assert_eq!(reference.repository(), "o/a");
    assert_eq!(reference.tag(), Some("v2"));
    assert_eq!(reference.manifest_selector(), format!("sha256:{digest}"));
}

#[test]
fn plain_reference_has_no_digest_and_uses_tag() {
    let reference: Reference = "alpine:3.19".parse().unwrap();
    assert!(reference.digest().is_none());
    assert_eq!(reference.manifest_selector(), "3.19");
}

#[test]
fn split_tag_explicit_tag() {
    let reference: Reference = "ubuntu:24.04".parse().unwrap();
    assert_eq!(reference.repository(), "library/ubuntu");
    assert_eq!(reference.tag(), Some("24.04"));
}

#[test]
fn split_tag_defaults_latest_when_absent() {
    assert_eq!("ubuntu".parse::<Reference>().unwrap().tag(), Some("latest"));
}

#[test]
fn split_tag_registry_port_is_not_a_tag() {
    let untagged: Reference = "localhost:5000/foo".parse().unwrap();
    assert_eq!(untagged.registry(), "localhost:5000");
    assert_eq!(untagged.tag(), Some("latest"));
    let tagged: Reference = "localhost:5000/foo:1.2".parse().unwrap();
    assert_eq!(tagged.registry(), "localhost:5000");
    assert_eq!(tagged.tag(), Some("1.2"));
}

#[test]
fn from_str_uses_reference_parser() {
    let reference: Reference = "localhost:5000/app:1".parse().unwrap();
    assert_eq!(reference.registry(), "localhost:5000");
    assert_eq!(reference.repository(), "app");
    assert_eq!(reference.tag(), Some("1"));
}
