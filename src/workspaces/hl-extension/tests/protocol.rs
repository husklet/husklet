//! Public contract of the extension protocol: framing, manifests, and the
//! handshake. Everything an untrusted peer can send is exercised here.

use hl_extension::{
    Activation, Capability, ChannelId, Compatibility, ExtensionName, Flags, Frame, Grant, Hello, Invalid, Kind, Limits,
    Malformed, Manifest, RelativePath, Resources, Welcome, PROTOCOL,
};

/// The document an extension image carries, with extra lines appended.
fn manifest_document(extra: &str) -> String {
    format!(
        "name = \"containers\"\n\
         display_name = \"Containers\"\n\
         version = \"1.0.0\"\n\
         protocol = {PROTOCOL}\n\
         capabilities = [\"container-read\"]\n\
         {extra}"
    )
}

#[test]
fn a_frame_survives_a_round_trip() {
    let frame = Frame::new(
        ChannelId::new(3),
        Kind::Request,
        b"{\"call\":\"containers.list\"}".to_vec(),
    );
    let bytes = frame.encode().expect("encoded");

    assert_eq!(bytes.len(), Frame::HEADER + frame.payload.len());
    let (decoded, consumed) = Frame::decode(&bytes).expect("valid").expect("complete");
    assert_eq!(decoded, frame);
    assert_eq!(consumed, bytes.len());
}

#[test]
fn a_stream_decodes_one_frame_at_a_time() {
    let first = Frame::new(ChannelId::new(1), Kind::Event, b"a".to_vec());
    let second = Frame::new(ChannelId::new(3), Kind::Event, b"bb".to_vec());
    let mut bytes = first.encode().expect("encoded");
    bytes.extend(second.encode().expect("encoded"));

    let (decoded, consumed) = Frame::decode(&bytes).expect("valid").expect("complete");
    assert_eq!(decoded, first);
    let (decoded, _) = Frame::decode(&bytes[consumed..]).expect("valid").expect("complete");
    assert_eq!(decoded, second);
}

#[test]
fn a_partial_frame_asks_for_more_rather_than_failing() {
    let frame = Frame::new(ChannelId::new(1), Kind::Event, b"payload".to_vec());
    let bytes = frame.encode().expect("encoded");

    assert_eq!(Frame::decode(&bytes[..4]).expect("valid"), None, "a short header waits");
    assert_eq!(
        Frame::decode(&bytes[..Frame::HEADER + 2]).expect("valid"),
        None,
        "a short payload waits"
    );
}

#[test]
fn an_oversize_declaration_is_refused_before_anything_is_reserved() {
    let mut header = Vec::new();
    let declared = Frame::PAYLOAD_LIMIT + 1;
    header.extend_from_slice(&(declared as u32).to_le_bytes());
    header.extend_from_slice(&1_u32.to_le_bytes());
    header.push(3);
    header.push(Flags::END.raw());
    header.extend_from_slice(&0_u16.to_le_bytes());

    assert_eq!(Frame::decode(&header), Err(Malformed::Oversize { declared }));
}

#[test]
fn an_unknown_kind_or_flag_or_reserved_bit_is_refused() {
    let frame = Frame::new(ChannelId::new(1), Kind::Event, b"x".to_vec());
    let bytes = frame.encode().expect("encoded");

    let mut unknown_kind = bytes.clone();
    unknown_kind[8] = 200;
    assert_eq!(Frame::decode(&unknown_kind), Err(Malformed::UnknownKind(200)));

    let mut unknown_flags = bytes.clone();
    unknown_flags[9] = 0b1000_0000;
    assert_eq!(Frame::decode(&unknown_flags), Err(Malformed::UnknownFlags(0b1000_0000)));

    let mut reserved = bytes;
    reserved[10] = 1;
    assert_eq!(
        Frame::decode(&reserved),
        Err(Malformed::Reserved),
        "the reserved field must stay available for a later encoding"
    );
}

#[test]
fn every_frame_kind_decodes_to_itself() {
    for kind in Kind::ALL {
        let bytes = Frame::new(ChannelId::CONTROL, *kind, Vec::new())
            .encode()
            .expect("encoded");
        let (decoded, _) = Frame::decode(&bytes).expect("valid").expect("complete");
        assert_eq!(decoded.kind, *kind);
    }
}

#[test]
fn a_manifest_round_trips_through_its_label() {
    let label = manifest_document("activation = \"workspace\"\n");
    let manifest = Manifest::parse(&label, PROTOCOL).expect("valid manifest");

    assert_eq!(manifest.name.as_str(), "containers");
    assert_eq!(manifest.activation, Activation::Workspace);
    assert!(manifest.capabilities.holds(Capability::ContainerRead));

    let again = Manifest::parse(&manifest.document().expect("serialized"), PROTOCOL).expect("valid");
    assert_eq!(again, manifest);
}

#[test]
fn an_unknown_manifest_field_is_refused_rather_than_ignored() {
    let label = manifest_document("sandbox = \"strict\"\n");
    let refused = Manifest::parse(&label, PROTOCOL).expect_err("refused");
    assert!(
        matches!(refused, Invalid::Malformed(_)),
        "an extension expecting a feature this host lacks must fail loudly, got {refused:?}"
    );
}

#[test]
fn a_manifest_for_another_protocol_names_both_versions() {
    let label = manifest_document("").replace(
        &format!("protocol = {PROTOCOL}"),
        &format!("protocol = {}", PROTOCOL + 7),
    );
    assert_eq!(
        Manifest::parse(&label, PROTOCOL),
        Err(Invalid::Protocol {
            declared: PROTOCOL + 7,
            supported: PROTOCOL
        })
    );
}

#[test]
fn a_manifest_cannot_use_a_capability_it_did_not_declare() {
    let interface = manifest_document("[interface]\ntab_title = \"Containers\"\n");
    assert_eq!(
        Manifest::parse(&interface, PROTOCOL),
        Err(Invalid::Undeclared(Capability::Interface))
    );

    let roots = manifest_document("filesystem_roots = [\"logs\"]\n");
    assert_eq!(
        Manifest::parse(&roots, PROTOCOL),
        Err(Invalid::Undeclared(Capability::FilesystemRead))
    );
}

#[test]
fn pane_providers_are_named_and_require_interface_authority() {
    let providers = "[[pane_providers]]\nid = \"database\"\ntitle = \"Postgres\"\nicon = \"database-symbolic\"\n";
    assert_eq!(
        Manifest::parse(&manifest_document(providers), PROTOCOL),
        Err(Invalid::Undeclared(Capability::Interface))
    );

    let document = manifest_document(providers).replace(
        "capabilities = [\"container-read\"]",
        "capabilities = [\"container-read\", \"interface\"]",
    );
    let manifest = Manifest::parse(&document, PROTOCOL).expect("provider manifest");
    assert_eq!(manifest.pane_providers[0].id.as_str(), "database");
    assert_eq!(manifest.pane_providers[0].title, "Postgres");
    assert_eq!(manifest.pane_providers[0].icon.as_deref(), Some("database-symbolic"));
}

#[test]
fn duplicate_or_untitled_pane_providers_are_refused() {
    let prefix = "[[pane_providers]]\nid = \"database\"\ntitle = \"Postgres\"\n";
    let duplicate = format!("{prefix}[[pane_providers]]\nid = \"database\"\ntitle = \"Logs\"\n");
    let document = manifest_document(&duplicate).replace(
        "capabilities = [\"container-read\"]",
        "capabilities = [\"container-read\", \"interface\"]",
    );
    assert_eq!(Manifest::parse(&document, PROTOCOL), Err(Invalid::PaneProviders));

    let blank = manifest_document("[[pane_providers]]\nid = \"database\"\ntitle = \"  \"\n").replace(
        "capabilities = [\"container-read\"]",
        "capabilities = [\"container-read\", \"interface\"]",
    );
    assert_eq!(Manifest::parse(&blank, PROTOCOL), Err(Invalid::PaneProviders));
}

#[test]
fn a_manifest_path_that_escapes_is_refused_with_the_manifest() {
    let label = format!(
        "{{\"name\":\"x\",\"display_name\":\"X\",\"version\":\"1\",\"protocol\":{PROTOCOL},\
          \"capabilities\":[\"filesystem-read\"],\"filesystem_roots\":[\"../../etc\"]}}"
    );
    assert!(matches!(Manifest::parse(&label, PROTOCOL), Err(Invalid::Malformed(_))));
}

#[test]
fn an_over_long_manifest_is_refused_before_it_is_parsed() {
    let label = "x".repeat(Manifest::LIMIT + 1);
    assert_eq!(
        Manifest::parse(&label, PROTOCOL),
        Err(Invalid::TooLong(Manifest::LIMIT + 1))
    );
}

#[test]
fn extension_names_stay_safe_as_directory_and_container_components() {
    for refused in ["", "Containers", "my extension", "../escape", ".hidden", "-lead", "a/b"] {
        assert!(ExtensionName::new(refused).is_err(), "{refused:?} must be refused");
    }
    for accepted in ["containers", "postgres.gui", "my-ext_2"] {
        assert!(ExtensionName::new(accepted).is_ok(), "{accepted:?} must be accepted");
    }
}

#[test]
fn a_resource_request_is_clamped_and_the_excess_is_reported() {
    let greedy = Resources {
        memory_mb: 64 * 1024,
        cpus: 64,
        process_count: 100_000,
    };
    assert!(
        greedy.exceeds_ceiling(),
        "install must be able to say it asked for more"
    );

    let granted = greedy.clamp();
    assert_eq!(granted.memory_mb, Resources::CEILING_MEMORY_MB);
    assert_eq!(granted.cpus, Resources::CEILING_CPUS);
    assert_eq!(granted.process_count, Resources::CEILING_PROCESS_COUNT);

    let modest = Resources {
        memory_mb: 128,
        cpus: 1,
        process_count: 16,
    };
    assert_eq!(
        modest.clamp(),
        modest,
        "a request within the ceiling is granted as asked"
    );
    assert!(!modest.exceeds_ceiling());
}

#[test]
fn the_host_states_the_grant_before_the_extension_asks_for_anything() {
    let welcome = Welcome {
        protocol: PROTOCOL,
        host: "0.1.0".into(),
        workspace: "dev".into(),
        peer: ExtensionName::new("containers").expect("name"),
        granted: Grant::new([Capability::ContainerRead]),
        limits: Limits::default(),
    };

    let encoded = serde_json::to_string(&welcome).expect("serialized");
    let decoded: Welcome = serde_json::from_str(&encoded).expect("deserialized");

    assert_eq!(decoded, welcome);
    assert!(decoded.granted.holds(Capability::ContainerRead));
    assert!(
        !decoded.granted.holds(Capability::ContainerControl),
        "an extension must learn what it lacks without probing for it"
    );
    assert_eq!(decoded.limits.payload_limit, Frame::PAYLOAD_LIMIT);
}

#[test]
fn a_hello_naming_another_extension_is_not_taken_as_identity() {
    let hello: Hello = serde_json::from_str(&format!(
        "{{\"protocol\":{PROTOCOL},\"name\":\"impersonator\",\"features\":[]}}"
    ))
    .expect("deserialized");

    // The socket names the extension; this field is diagnostic only. The test
    // records that a listener must compare, never adopt, the claimed name.
    let listener = ExtensionName::new("containers").expect("name");
    assert_ne!(hello.name, listener, "a mismatch is fatal, not an identity change");
}

#[test]
fn silence_is_reported_differently_from_a_wrong_version() {
    assert!(Compatibility::of(PROTOCOL).is_compatible());
    assert!(!Compatibility::Unknown.is_compatible());
    assert_ne!(
        Compatibility::Unknown,
        Compatibility::of(PROTOCOL + 1),
        "an unfinished handshake must not be logged as a version failure"
    );
}

#[test]
fn a_wire_path_cannot_bypass_its_construction_rules() {
    let refused: Result<RelativePath, _> = serde_json::from_str("\"../../etc/shadow\"");
    assert!(refused.is_err());

    let accepted: RelativePath = serde_json::from_str("\"logs/app.log\"").expect("valid");
    let root = RelativePath::new("logs").expect("root");
    assert!(accepted.within(&root));
}

#[test]
fn every_manifest_this_repository_ships_is_one_a_host_accepts() {
    // The documents are the real ones, read from where an image copies them.
    // A manifest that stopped parsing would otherwise only be discovered by
    // building an image and watching an extension refuse to start.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..");
    let shipped = [
        root.join("extensions/storybook/extension.toml"),
        root.join("src/apps/extension/extension.toml"),
    ];

    for path in shipped {
        let document =
            std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let manifest = Manifest::parse(&document, PROTOCOL)
            .unwrap_or_else(|error| panic!("{} is not a manifest a host accepts: {error}", path.display()));
        assert!(
            !manifest.capabilities.is_empty(),
            "{} asks for nothing, so it could not do anything",
            path.display()
        );
        assert_eq!(
            Manifest::parse(&manifest.document().expect("written"), PROTOCOL).expect("re-read"),
            manifest,
            "{} does not survive being written back",
            path.display()
        );
    }
}
