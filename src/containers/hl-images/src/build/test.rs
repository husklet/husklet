use super::{
    Account, Assignments, Base, CacheSharing, Command, CopySource, Duration, Healthcheck, InstructionParser,
    OwnershipSpec, Platform, Recipe, RunMount, Source, Step, Words, WorkingDirectory,
};
use std::collections::BTreeMap;

#[path = "test_metadata.rs"]
mod metadata;
#[test]
fn parses_build_arguments_and_runtime_config() {
    let supplied = BTreeMap::from([
        ("BASE".into(), "alpine:latest".into()),
        ("VALUE".into(), "override".into()),
    ]);
    let recipe = Recipe::parse_with(
        "ARG BASE\nFROM ${BASE}\nARG VALUE=default\nENV RESULT=${VALUE}\nRUN echo $VALUE\nCMD echo $RESULT\n",
        &supplied,
        None,
    )
    .unwrap();
    assert_eq!(recipe.selected, 0);
    assert!(
        matches!(&recipe.stages[0].base, Base::Image(image) if image.to_string() == "docker.io/library/alpine:latest")
    );
    assert_eq!(recipe.stages[0].runtime.environment["RESULT"], "override");
    assert!(matches!(&recipe.stages[0].steps[0], Step::Run { environment, .. } if environment["VALUE"] == "override"));
    assert_eq!(
        recipe.stages[0]
            .history
            .iter()
            .map(|entry| (entry.created_by.as_deref().unwrap(), entry.empty_layer))
            .collect::<Vec<_>>(),
        [
            ("FROM ${BASE}", true),
            ("ARG VALUE=default", true),
            ("ENV RESULT=${VALUE}", true),
            ("RUN echo $VALUE", false),
            ("CMD echo $RESULT", true),
        ]
    );
}

#[test]
fn build_argument_declarations_apply_defaults_and_supplied_values() {
    let supplied = BTreeMap::from([
        ("BARE".into(), "supplied".into()),
        ("OVERRIDE".into(), "replacement".into()),
    ]);
    let recipe = Recipe::parse_with(
        "FROM alpine\nARG UNSET\nARG BARE\nARG DEFAULT=kept\nARG OVERRIDE=original\nRUN true\n",
        &supplied,
        None,
    )
    .unwrap();
    let Step::Run { environment, .. } = &recipe.stages[0].steps[0] else {
        panic!("expected RUN");
    };
    assert!(!environment.contains_key("UNSET"));
    assert_eq!(environment["BARE"], "supplied");
    assert_eq!(environment["DEFAULT"], "kept");
    assert_eq!(environment["OVERRIDE"], "replacement");
    assert!(Recipe::parse("FROM alpine\nARG NAME extra\n").is_err());
}

#[test]
fn pre_from_argument_does_not_leak_into_stage_without_redeclaration() {
    let recipe =
        Recipe::parse("ARG SECRET=global\nFROM alpine\nENV LEAK=$SECRET\nARG SECRET\nENV DECLARED=$SECRET\n").unwrap();
    assert_eq!(recipe.stages[0].runtime.environment["LEAK"], "");
    assert_eq!(recipe.stages[0].runtime.environment["DECLARED"], "global");
}

#[test]
fn expands_docker_argument_defaults_alternates_and_unset_values() {
    let supplied = BTreeMap::from([("EMPTY".into(), String::new()), ("SET".into(), "present".into())]);
    let recipe = Recipe::parse_with(
            "ARG BASE\nARG EMPTY\nARG SET\nFROM ${BASE:-alpine:latest}\nARG EMPTY\nARG SET\nENV A=${MISSING-default} B=${EMPTY:-fallback} C=${SET:+alternate} D=$MISSING E=${EMPTY-default} F=${MISSING+alternate} G=${SET+alternate}\nCOPY ${MISSING:-source} /target\n",
            &supplied,
            None,
        )
        .unwrap();
    let stage = &recipe.stages[0];
    assert_eq!(stage.runtime.environment["A"], "default");
    assert_eq!(stage.runtime.environment["B"], "fallback");
    assert_eq!(stage.runtime.environment["C"], "alternate");
    assert_eq!(stage.runtime.environment["D"], "");
    assert_eq!(stage.runtime.environment["E"], "");
    assert_eq!(stage.runtime.environment["F"], "");
    assert_eq!(stage.runtime.environment["G"], "alternate");
    assert!(matches!(
        &stage.steps[0],
        Step::Copy { sources, .. } if sources == &[Source::Local("source".into())]
    ));
}

#[test]
fn parses_multistage_copy_and_target() {
    let recipe = Recipe::parse_with(
        "FROM alpine AS build\nRUN echo x > /x\nFROM alpine AS final\nCOPY --from=build /x /x\n",
        &BTreeMap::new(),
        Some("final"),
    )
    .unwrap();
    assert_eq!(recipe.selected, 1);
    assert!(matches!(recipe.stages[1].base, Base::Image(_)));
    assert!(matches!(
        &recipe.stages[1].steps[0],
        Step::Copy {
            from: Some(CopySource::Stage(0)),
            ..
        }
    ));
    assert!(Recipe::parse_with("FROM alpine\nCOPY a b /x\n", &BTreeMap::new(), None).is_err());
}

#[test]
fn parses_external_image_copy_source() {
    let recipe =
        Recipe::parse("FROM alpine AS build\nFROM alpine\nCOPY --from=example/tools:1 /bin/tool /tool\n").unwrap();
    assert!(matches!(
        &recipe.stages[1].steps[0],
        Step::Copy {
            from: Some(CopySource::Image(reference)),
            ..
        } if reference.to_string() == "docker.io/example/tools:1"
    ));
    assert!(Recipe::parse("FROM alpine\nCOPY --from= /x /x\n").is_err());
}

#[test]
fn expands_automatic_platform_arguments_and_from_selector() {
    let build = Platform::linux_arm64();
    let target = Platform::new("linux", "arm", Some("v7".into()));
    let recipe = Recipe::parse_with_platforms(
            "FROM --platform=$BUILDPLATFORM alpine AS tools\nFROM --platform=$TARGETPLATFORM alpine\nARG TARGETPLATFORM\nARG TARGETARCH\nARG TARGETVARIANT\nARG BUILDARCH\nENV TARGET=$TARGETPLATFORM ARCH=$TARGETARCH VARIANT=$TARGETVARIANT BUILD=$BUILDARCH\n",
            &BTreeMap::new(),
            None,
            Some(&build),
            Some(&target),
        )
        .unwrap();
    assert_eq!(recipe.stages[0].platform, Some(build));
    assert_eq!(recipe.stages[1].platform, Some(target));
    assert_eq!(recipe.stages[1].runtime.environment["TARGET"], "linux/arm/v7");
    assert_eq!(recipe.stages[1].runtime.environment["ARCH"], "arm");
    assert_eq!(recipe.stages[1].runtime.environment["VARIANT"], "v7");
    assert_eq!(recipe.stages[1].runtime.environment["BUILD"], "arm64");
    assert!(Recipe::parse("FROM --platform=darwin/arm64 alpine\n").is_err());
}

#[test]
fn parses_typed_run_cache_and_read_only_bind_mounts() {
    let recipe = Recipe::parse(
            "FROM alpine AS source\nRUN echo x > /x\nFROM alpine\nRUN --mount=type=cache,id=compile,target=/cache,sharing=locked --mount=type=bind,from=source,source=/x,target=/input,ro cat /input > /result\n",
        )
        .unwrap();
    let Step::Run { command, mounts, .. } = &recipe.stages[1].steps[0] else {
        panic!("expected RUN step");
    };
    assert_eq!(command, "cat /input > /result");
    assert_eq!(
        mounts,
        &[
            RunMount::Cache {
                id: Some("compile".into()),
                target: "/cache".into(),
                sharing: CacheSharing::Locked,
            },
            RunMount::Bind {
                from: Some(0),
                source: "/x".into(),
                target: "/input".into(),
            },
        ]
    );
    for invalid in [
        "RUN --mount=type=secret,target=/run/secret true",
        "RUN --mount=type=bind,target=../escape true",
        "RUN --mount=type=bind,target=/src,rw true",
        "RUN --mount=type=cache,target=/cache,mode=0777 true",
    ] {
        assert!(
            Recipe::parse(&format!("FROM alpine\n{invalid}\n")).is_err(),
            "accepted {invalid:?}"
        );
    }
}

#[test]
fn parses_copy_excludes_parents_and_link_policy() {
    let recipe = Recipe::parse(
        "FROM alpine\nCOPY --exclude=*.tmp --exclude=private/ --parents --link=false ./src/file /root/\n",
    )
    .unwrap();
    assert!(matches!(
        &recipe.stages[0].steps[0],
        Step::Copy {
            excludes,
            parents: true,
            ..
        } if excludes == &["*.tmp", "private"]
    ));
    for invalid in [
        "COPY --exclude= source /root/",
        "COPY --parents source /root",
        "COPY --link source /root/",
        "COPY --link=true source /root/",
    ] {
        assert!(
            Recipe::parse(&format!("FROM alpine\n{invalid}\n")).is_err(),
            "accepted {invalid:?}"
        );
    }
    assert!(Recipe::parse("FROM alpine\nCOPY --link=false source /root/\n").is_ok());
}

#[test]
fn parses_remote_add_checksum_and_rejects_git_sources() {
    let checksum = "a".repeat(64);
    let recipe = Recipe::parse(&format!(
        "FROM alpine\nADD --checksum=sha256:{checksum} https://example.test/archive.tar /download\n"
    ))
    .unwrap();
    assert!(matches!(
        &recipe.stages[0].steps[0],
        Step::Copy {
            sources,
            checksum: Some(value),
            unpack: true,
            ..
        } if sources == &[Source::Remote("https://example.test/archive.tar".into())]
            && value == &format!("sha256:{checksum}")
    ));
    for invalid in [
        "COPY https://example.test/file /file",
        "ADD git://example.test/repository /source",
        "ADD https://example.test/repository.git /source",
        "ADD --checksum=md5:abcd https://example.test/file /file",
    ] {
        assert!(
            Recipe::parse(&format!("FROM alpine\n{invalid}\n")).is_err(),
            "accepted {invalid:?}"
        );
    }
}

#[test]
fn rejects_malformed_configuration() {
    assert!(Recipe::parse("FROM alpine\nCMD [\"ok\",1]\n").is_err());
    for owner in [":2", "1:", "user:group:extra", "bad/name"] {
        assert!(Recipe::parse(&format!("FROM alpine\nCOPY --chown={owner} x /x\n")).is_err());
    }
    assert!(Recipe::parse_with("FROM alpine\n", &BTreeMap::new(), Some("missing")).is_err());
}

#[test]
fn preserves_shell_and_exec_command_forms() {
    let recipe = Recipe::parse("FROM alpine AS exec\nCMD [\"echo\",\"hi\"]\nFROM alpine\nCMD [ -f /x ]\n").unwrap();
    assert_eq!(recipe.stages[0].runtime.command, ["echo", "hi"]);
    assert_eq!(recipe.stages[1].runtime.command, ["/bin/sh", "-c", "[ -f /x ]"]);
    assert!(Recipe::parse("FROM alpine\nCMD [\"echo\", 1]\n").is_err());
    assert!(Recipe::parse("FROM alpine\nCMD [not json]\n").is_err());
}

#[test]
fn command_healthcheck_and_duration_grammars_preserve_boundaries() {
    let exec: Vec<String> = r#"["echo","ok"]"#.parse::<Command>().unwrap().into();
    assert_eq!(exec, ["echo", "ok"]);
    let shell: Vec<String> = "[ -f /ready ]".parse::<Command>().unwrap().into();
    assert_eq!(shell, ["/bin/sh", "-c", "[ -f /ready ]"]);
    assert!(r#"["echo",1]"#.parse::<Command>().is_err());

    for (value, nanos) in [
        ("1ns", 1),
        ("1us", 1_000),
        ("1µs", 1_000),
        ("1ms", 1_000_000),
        ("1s", 1_000_000_000),
        ("1m", 60_000_000_000),
        ("1h", 3_600_000_000_000),
    ] {
        assert_eq!(u64::from(value.parse::<Duration>().unwrap()), nanos);
    }
    for invalid in ["1", "-1s", "NaNs", "1day", "1e999h"] {
        assert!(invalid.parse::<Duration>().is_err(), "accepted {invalid:?}");
    }

    let health: serde_json::Value = "--interval=1s --interval=2s --timeout=500ms --retries=3 CMD [\"check\",\"now\"]"
        .parse::<Healthcheck>()
        .unwrap()
        .into();
    assert_eq!(health["Interval"], 2_000_000_000_u64);
    assert_eq!(health["Timeout"], 500_000_000_u64);
    assert_eq!(health["Retries"], 3_u64);
    assert_eq!(health["Test"], serde_json::json!(["CMD", "check", "now"]));
    assert!("--timeout=1s NONE".parse::<Healthcheck>().is_err());
    assert!("CMD [\"check\",1]".parse::<Healthcheck>().is_err());
}

#[test]
fn parses_escape_directive_and_continuation() {
    let recipe = Recipe::parse("# escape=`\nFROM alpine\nRUN echo first `\n && echo second\n").unwrap();
    assert!(matches!(
        &recipe.stages[0].steps[0],
        Step::Run { command, .. } if command == "echo first  && echo second"
    ));
}

#[test]
fn lexical_entities_preserve_quotes_escapes_and_long_continuations() {
    assert_eq!(
        Words::new(r#"one "two three" four\ five 'six seven'"#).parse(),
        ["one", "two three", "four five", "six seven"]
    );
    assert_eq!(
        Assignments::new(r#"A="one two" B=three\ four"#).parse().unwrap(),
        [("A".into(), "one two".into()), ("B".into(), "three four".into())]
    );
    assert!(Assignments::new(r#"A="unterminated"#).parse().is_err());
    assert_eq!(
        WorkingDirectory::new("/workspace/deep")
            .resolve("../../../../safe/./target")
            .unwrap(),
        "/safe/target"
    );

    let mut source = String::from("# escape=`\nFROM alpine\nRUN first `\n");
    for _ in 0..2_048 {
        source.push_str("middle `\n");
    }
    source.push_str("last\n");
    let instructions = InstructionParser::new(&source).parse().unwrap();
    assert_eq!(instructions.len(), 2);
    assert!(instructions[1].value.starts_with("first  middle"));
    assert!(instructions[1].value.ends_with(" last"));
}

#[test]
fn rejects_malformed_dockerfile_structure() {
    for dockerfile in ["RUN echo \\", "# escape=x\nRUN x", "FROM"] {
        assert!(
            Recipe::parse(dockerfile).is_err(),
            "accepted malformed Dockerfile {dockerfile:?}"
        );
    }
}

#[test]
fn rejects_unterminated_variable_substitution() {
    for dockerfile in [
        "FROM ${BASE\n",
        "FROM alpine\nENV VALUE=${MISSING\n",
        "FROM alpine\nWORKDIR /work/${MISSING\n",
    ] {
        let error = Recipe::parse(dockerfile).unwrap_err();
        assert!(
            error.to_string().contains("unterminated variable substitution"),
            "unexpected error for {dockerfile:?}: {error}"
        );
    }
}

#[test]
fn normalizes_workdir_parent_components() {
    let recipe =
        Recipe::parse("FROM alpine\nWORKDIR /workspace/one\nWORKDIR ../two\nWORKDIR ./three/../four\n").unwrap();
    assert_eq!(recipe.stages[0].runtime.working_directory, "/workspace/two/four");
}

#[test]
fn rejects_malformed_environment_and_label_assignments() {
    let recipe = Recipe::parse("FROM alpine\nENV A=1 B=two\nLABEL owner=husklet\n").unwrap();
    assert_eq!(recipe.stages[0].runtime.environment["A"], "1");
    assert_eq!(recipe.stages[0].runtime.environment["B"], "two");
    assert_eq!(recipe.stages[0].labels["owner"], "husklet");
    for dockerfile in ["FROM alpine\nLABEL A=1 broken\n", "FROM alpine\nENV A=\"unterminated\n"] {
        assert!(Recipe::parse(dockerfile).is_err(), "accepted {dockerfile:?}");
    }
}

#[test]
fn parses_numeric_copy_ownership() {
    let recipe = Recipe::parse("FROM alpine\nCOPY --chown=12:34 one /one\nADD --chown=56 two /two\n").unwrap();
    assert!(matches!(
        recipe.stages[0].steps[0],
        Step::Copy {
            ownership: Some(OwnershipSpec {
                user: Account::Id(12),
                group: Some(Account::Id(34))
            }),
            ..
        }
    ));
    assert!(Recipe::parse("FROM alpine\nCOPY --chown=user --chown=1:2 one /one\n").is_err());
    assert!(matches!(
        recipe.stages[0].steps[1],
        Step::Copy {
            ownership: Some(OwnershipSpec {
                user: Account::Id(56),
                group: None
            }),
            ..
        }
    ));
}

#[test]
fn parses_image_metadata_instructions() {
    let recipe = Recipe::parse(
            "FROM alpine\nONBUILD RUN touch /ready\nEXPOSE 80 53/udp\nVOLUME [\"/data\"]\nHEALTHCHECK --interval=5s --retries=3 CMD test -f /ready\n",
        )
        .unwrap();
    let stage = &recipe.stages[0];
    assert_eq!(stage.onbuild, ["RUN touch /ready"]);
    assert_eq!(
        stage.exposed_ports.iter().cloned().collect::<Vec<_>>(),
        ["53/udp", "80/tcp"]
    );
    assert_eq!(stage.volumes.iter().cloned().collect::<Vec<_>>(), ["/data"]);
    assert_eq!(stage.healthcheck.as_ref().unwrap()["Interval"], 5_000_000_000_u64);
    assert_eq!(stage.healthcheck.as_ref().unwrap()["Retries"], 3);
}
