use std::{collections::BTreeSet, sync::Arc};

use hl_engine::{
    extension::{
        BindAccess, DirectoryEntry, ExtensionConfig, ExtensionSpec, Feature, FileEntry, FileSource,
        HostBindEntry, Metadata, NamespaceEntry, ProviderId, SymlinkEntry,
    },
    spec::{FilesystemFeature, Version},
    Engine, Guest, MachineSpec,
};

#[test]
fn container_dependency_can_negotiate_generic_namespace_projection() {
    let engine = Engine::new();
    let host = std::env::temp_dir().join(format!("hl-container-host-bind-{}", std::process::id()));
    std::fs::write(&host, b"host-value").unwrap();
    assert!(engine
        .capabilities()
        .filesystems
        .features
        .contains(&FilesystemFeature::ProjectedNamespace));
    let mut spec = MachineSpec::new(Guest::Aarch64, "/bin/true");
    spec.extensions.push(ExtensionSpec {
        provider: ProviderId::new("engine.namespace").unwrap(),
        version: Version::new(1, 0),
        required: true,
        required_features: BTreeSet::from([
            Feature::new("host-bind-read-only").unwrap(),
            Feature::new("mutable-files").unwrap(),
        ]),
        optional_features: BTreeSet::new(),
        config: ExtensionConfig::empty("engine.namespace/v1"),
        namespace: vec![
            NamespaceEntry::Directory(DirectoryEntry {
                path: "/opt/provider".into(),
                metadata: Metadata {
                    mode: 0o755,
                    uid: 0,
                    gid: 0,
                },
            }),
            NamespaceEntry::File(FileEntry {
                path: "/opt/provider/config".into(),
                metadata: Metadata {
                    mode: 0o444,
                    uid: 0,
                    gid: 0,
                },
                source: FileSource::Immutable(Arc::from(&b"value"[..])),
            }),
            NamespaceEntry::Symlink(SymlinkEntry {
                path: "/opt/provider/current".into(),
                target: "config".into(),
                uid: 0,
                gid: 0,
            }),
            NamespaceEntry::HostBind(HostBindEntry {
                path: "/opt/provider/host".into(),
                host: host.clone(),
                access: BindAccess::ReadOnly,
            }),
            NamespaceEntry::File(FileEntry {
                path: "/opt/provider/state".into(),
                metadata: Metadata {
                    mode: 0o600,
                    uid: 0,
                    gid: 0,
                },
                source: FileSource::Mutable(Arc::from(&b"initial"[..])),
            }),
        ],
        services: Vec::new(),
        memory: Vec::new(),
        environment: Vec::new(),
    });
    let validation = engine.validate(&spec).unwrap();
    assert_eq!(validation.selected_extensions.len(), 1);
    assert_eq!(validation.resources.namespace_entries, 5);
    std::fs::remove_file(host).unwrap();
}
