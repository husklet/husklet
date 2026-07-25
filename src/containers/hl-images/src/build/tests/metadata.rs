use super::*;
use crate::build::Changes;

#[test]
fn commit_changes_apply_runtime_and_image_metadata_and_reject_unknowns() {
    let mut metadata = crate::Metadata {
        platform: crate::Platform::linux_arm64(),
        created: None,
        author: None,
        labels: BTreeMap::new(),
        history: Vec::new(),
        runtime: crate::RuntimeConfig {
            entrypoint: Vec::new(),
            command: Vec::new(),
            environment: BTreeMap::new(),
            working_directory: "/".into(),
            user: String::new(),
        },
        onbuild: Vec::new(),
        exposed_ports: std::collections::BTreeSet::default(),
        volumes: std::collections::BTreeSet::default(),
        healthcheck: None,
        stop_signal: None,
    };
    let changes = [
        r#"CMD ["echo","ok"]"#,
        "ENTRYPOINT /bin/sh -c",
        "ENV A=one B=two",
        "EXPOSE 80 53/udp",
        "LABEL tier=api",
        "ONBUILD RUN true",
        "STOPSIGNAL SIGTERM",
        "USER 1000:1000",
        r#"VOLUME ["/data"]"#,
        "WORKDIR /workspace",
    ]
    .map(str::to_owned);
    Changes::new(&changes).apply(&mut metadata).unwrap();
    assert_eq!(metadata.runtime.command, ["echo", "ok"]);
    assert_eq!(metadata.runtime.entrypoint, ["/bin/sh", "-c", "/bin/sh -c"]);
    assert_eq!(metadata.runtime.environment["A"], "one");
    assert!(metadata.exposed_ports.contains("80/tcp"));
    assert!(metadata.volumes.contains("/data"));
    assert_eq!(metadata.runtime.working_directory, "/workspace");
    assert_eq!(metadata.history.len(), changes.len());
    assert_eq!(
        metadata
            .history
            .iter()
            .map(|entry| entry.created_by.as_deref().unwrap())
            .collect::<Vec<_>>(),
        changes.iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert!(metadata.history.iter().all(|entry| entry.empty_layer));
    assert!(Changes::new(&["FROM alpine".into()])
        .apply(&mut metadata)
        .is_err());
}
