use super::super::cache::build_cache_key;
use super::super::{BaseImages, BuildNetwork};
use hl_container::{ContainerSpec, Process};
use std::collections::BTreeMap;

#[test]
fn base_image_policy_refreshes_present_images_only_when_requested() {
    assert!(BaseImages::Local.requires_pull(false));
    assert!(!BaseImages::Local.requires_pull(true));
    assert!(BaseImages::Pull.requires_pull(false));
    assert!(BaseImages::Pull.requires_pull(true));
}

#[test]
fn build_network_policy_maps_to_valid_container_specs() {
    let root = tempfile::tempdir().unwrap();
    let spec = || ContainerSpec::from_directory(root.path(), Process::new("/bin/true"));
    let default = BuildNetwork::Default.container(spec());
    assert!(!default.isolation.network_isolated);
    assert_eq!(default.network_mode, hl_container::NetworkMode::Automatic);

    let none = BuildNetwork::None.container(spec());
    assert!(none.isolation.network_isolated);
    assert_eq!(none.network_mode, hl_container::NetworkMode::Automatic);

    let host = BuildNetwork::Host.container(spec());
    assert!(!host.isolation.network_isolated);
    assert_eq!(host.network_mode, hl_container::NetworkMode::Host);

    let named = BuildNetwork::Named("backend".into()).container(spec());
    assert!(!named.isolation.network_isolated);
    assert_eq!(named.network_mode, hl_container::NetworkMode::Automatic);
}

#[test]
fn build_cache_identity_is_deterministic() {
    let arguments = BTreeMap::from([("MODE".into(), "release".into())]);
    let context = [0x5a; 32];
    let first = build_cache_key(
        "FROM scratch\nRUN true\n",
        &arguments,
        Some("runtime"),
        context,
    );
    let second = build_cache_key(
        "FROM scratch\nRUN true\n",
        &arguments,
        Some("runtime"),
        context,
    );
    assert_eq!(first, second);
    assert_eq!(first.len(), 64);
    assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
}

#[test]
fn build_cache_identity_includes_parent_recipe() {
    let arguments = BTreeMap::new();
    let context = [0x5a; 32];
    assert_ne!(
        build_cache_key("FROM parent:a\nRUN true\n", &arguments, None, context),
        build_cache_key("FROM parent:b\nRUN true\n", &arguments, None, context),
    );
}

#[test]
fn build_cache_identity_includes_instruction_descriptor() {
    let arguments = BTreeMap::new();
    let context = [0x5a; 32];
    assert_ne!(
        build_cache_key("FROM scratch\nRUN echo first\n", &arguments, None, context),
        build_cache_key("FROM scratch\nRUN echo second\n", &arguments, None, context),
    );
}
