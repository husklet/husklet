use super::{Fields, ImageSelection, ListQuery, Prune, RemoveQuery, build_image_summaries, removal_conflicts};
use axum::http::StatusCode;
use std::collections::BTreeMap;

#[test]
fn image_queries_reject_meaningful_unknown_options_before_work() {
    for harmless in ["", "0", "false", "null", "[]", "{}"] {
        Fields::from(&BTreeMap::from([("FutureOption".into(), harmless.into())]))
            .reject("image test")
            .unwrap();
    }
    let fields = BTreeMap::from([("FutureOption".into(), "enabled".into())]);
    let error = Fields::from(&fields).reject("image test").unwrap_err();
    assert_eq!(error.status, StatusCode::NOT_IMPLEMENTED);

    let list = ListQuery {
        all: Some("1".into()),
        shared_size: Some("true".into()),
        ..ListQuery::default()
    };
    assert!(list.selection().unwrap().1);
    let remove = RemoveQuery {
        force: Some("true".into()),
        noprune: Some("true".into()),
        ..RemoveQuery::default()
    };
    assert!(remove.validate().unwrap());
}

#[test]
fn summaries_without_shared_size_visit_each_target_once() {
    let digest = format!("sha256:{}", "1".repeat(64));
    let record = |name: &str| {
        serde_json::from_value::<hl_images::Graph>(serde_json::json!({
            "target": {
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": digest,
                "size": 23
            },
            "names": [name.parse::<hl_images::Reference>().unwrap().to_string()],
            "created_at_ms": null,
            "labels": null,
            "build_cache": false,
            "metadata_known": false
        }))
        .unwrap()
    };
    let sizes = std::cell::Cell::new(0);
    let details = std::cell::Cell::new(0);
    let summaries = build_image_summaries(
        vec![record("example:first"), record("example:second")],
        false,
        |_| {
            sizes.set(sizes.get() + 1);
            Ok(101)
        },
        |_| panic!("shared descriptor accounting must be skipped"),
        |_| {
            details.set(details.get() + 1);
            Ok(BTreeMap::from([("tier".into(), "test".into())]))
        },
    )
    .unwrap();

    assert_eq!(sizes.get(), 1);
    assert_eq!(details.get(), 1);
    assert_eq!(summaries.len(), 1);
    assert_eq!(
        summaries[0].repo_tags,
        ["docker.io/library/example:first", "docker.io/library/example:second"]
    );
    assert_eq!(summaries[0].size, 101);
    assert_eq!(summaries[0].shared_size, -1);
    assert_eq!(summaries[0].labels.get("tier").map(String::as_str), Some("test"));
}

#[test]
fn summaries_with_shared_size_batch_unique_targets_and_propagate_reads() {
    let record = |name: &str, digit: char| {
        serde_json::from_value::<hl_images::Graph>(serde_json::json!({
            "target": {
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": format!("sha256:{}", digit.to_string().repeat(64)),
                "size": 23
            },
            "names": [name.parse::<hl_images::Reference>().unwrap().to_string()],
            "created_at_ms": null,
            "labels": null,
            "build_cache": false,
            "metadata_known": false
        }))
        .unwrap()
    };
    let usage_calls = std::cell::Cell::new(0);
    let details = std::cell::Cell::new(0);
    let summaries = build_image_summaries(
        vec![
            record("second:alias", '2'),
            record("first:tag", '1'),
            record("second:tag", '2'),
        ],
        true,
        |_| panic!("individual size walks must be skipped"),
        |unique| {
            usage_calls.set(usage_calls.get() + 1);
            assert_eq!(unique.len(), 2);
            Ok(unique
                .iter()
                .map(|target| {
                    (
                        target.digest().to_string(),
                        hl_images::ImageUsage { size: 200, shared: 75 },
                    )
                })
                .collect())
        },
        |_| {
            details.set(details.get() + 1);
            Ok(BTreeMap::new())
        },
    )
    .unwrap();

    assert_eq!(usage_calls.get(), 1);
    assert_eq!(details.get(), 2);
    assert_eq!(summaries.len(), 2);
    assert!(summaries.windows(2).all(|pair| pair[0].id < pair[1].id));
    assert_eq!(
        summaries[1].repo_tags,
        ["docker.io/library/second:alias", "docker.io/library/second:tag"]
    );
    assert!(
        summaries
            .iter()
            .all(|summary| summary.size == 200 && summary.shared_size == 75)
    );

    let error = build_image_summaries(
        vec![record("broken:tag", '3')],
        false,
        |_| Err(hl_images::Error::InvalidMetadata("unreadable target".into())),
        |_| panic!("shared usage must remain skipped"),
        |_| panic!("details must not run after a size failure"),
    )
    .unwrap_err();
    assert!(matches!(error, hl_images::Error::InvalidMetadata(message) if message == "unreadable target"));
}

#[test]
fn summaries_project_tagged_and_dangling_graphs_without_inventing_names() {
    let descriptor = |digit: char| {
        serde_json::from_value::<hl_images::Descriptor>(serde_json::json!({
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": format!("sha256:{}", digit.to_string().repeat(64)),
            "size": 23
        }))
        .unwrap()
    };
    let graph = |digit: char, names: &[&str], build_cache: bool| hl_images::Graph {
        target: descriptor(digit),
        names: names
            .iter()
            .map(|name| name.parse::<hl_images::Reference>().unwrap().to_string())
            .collect(),
        created_at_ms: None,
        labels: None,
        build_cache,
        metadata_known: false,
    };

    let summaries = build_image_summaries(
        vec![
            graph('1', &["example:tagged"], false),
            graph('2', &[], false),
            graph('3', &["hl-build-cache/step:v1"], true),
        ],
        false,
        |_| Ok(101),
        |_| panic!("shared descriptor accounting must be skipped"),
        |target| {
            Ok(BTreeMap::from([(
                "kind".into(),
                if target.digest().to_string().ends_with('1') {
                    "tagged".into()
                } else {
                    "dangling".into()
                },
            )]))
        },
    )
    .unwrap();

    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].repo_tags, ["docker.io/library/example:tagged"]);
    assert!(summaries[1].repo_tags.is_empty());
    assert!(summaries.iter().all(|summary| summary.shared_size == -1));

    let selection = |value: &str| {
        ListQuery {
            filters: Some(format!(r#"{{"dangling":["{value}"]}}"#)),
            ..ListQuery::default()
        }
        .selection()
        .unwrap()
        .0
    };
    let dangling = selection("true");
    let tagged = selection("false");
    assert_eq!(summaries.iter().filter(|summary| dangling.matches(summary)).count(), 1);
    assert_eq!(summaries.iter().filter(|summary| tagged.matches(summary)).count(), 1);

    let filtered = |filters: &str| {
        ListQuery {
            filters: Some(filters.into()),
            ..ListQuery::default()
        }
        .selection()
        .unwrap()
        .0
    };
    let reference = filtered(r#"{"reference":["*example:tagged"]}"#);
    let label = filtered(r#"{"label":["kind=dangling"]}"#);
    assert_eq!(summaries.iter().filter(|summary| reference.matches(summary)).count(), 1);
    assert_eq!(summaries.iter().filter(|summary| label.matches(summary)).count(), 1);

    for all in [Some("false".into()), Some("true".into())] {
        let query = ListQuery {
            all,
            ..ListQuery::default()
        };
        let (selection, _) = query.selection().unwrap();
        assert_eq!(summaries.iter().filter(|summary| selection.matches(summary)).count(), 2);
    }
}

#[test]
fn image_reference_wildcards_match_complete_names() {
    assert!(ImageSelection::wildcard("alpine:*", "alpine:3.20"));
    assert!(ImageSelection::wildcard("*/api:?", "team/api:1"));
    assert!(!ImageSelection::wildcard("alpine", "alpine:latest"));
    assert!(!ImageSelection::wildcard("*/api:?", "team/api:10"));
}

#[test]
fn image_remove_conflicts_only_when_the_target_would_become_unavailable() {
    assert!(!removal_conflicts(false, false, 2, [false]));
    assert!(removal_conflicts(false, false, 1, [false]));
    assert!(!removal_conflicts(true, false, 1, [false]));
    assert!(removal_conflicts(true, false, 2, [true]));
    assert!(!removal_conflicts(false, false, 1, []));
    assert!(removal_conflicts(false, true, 2, []));
    assert!(!removal_conflicts(true, true, 2, []));
}

#[test]
fn system_prune_preserves_all_and_filtered_operations() {
    assert!(matches!(Prune::parse(None).unwrap(), Prune::All));
    assert!(matches!(
        Prune::parse(Some(r#"{"until":["2"],"label":["stage=build"]}"#)).unwrap(),
        Prune::Selected { until: Some(2_000), .. }
    ));
    assert!(Prune::parse(Some(r#"{"until":["1","2"]}"#)).is_err());
}

#[test]
fn image_prune_defaults_to_dangling_and_accepts_metadata_filters() {
    let Prune::Selected { values, until } = Prune::image(None).unwrap() else {
        panic!("image prune must use a bounded selection")
    };
    assert_eq!(values.get("dangling").unwrap(), &["true"]);
    assert_eq!(until, None);

    let Prune::Selected { values, until } = Prune::image(Some(
        r#"{"dangling":["true"],"until":["2"],"label":["stage=build"],"label!":["keep"]}"#,
    ))
    .unwrap() else {
        panic!("filtered image prune must use a bounded selection")
    };
    assert_eq!(values.get("dangling").unwrap(), &["true"]);
    assert_eq!(values.get("label").unwrap(), &["stage=build"]);
    assert_eq!(values.get("label!").unwrap(), &["keep"]);
    assert_eq!(until, Some(2_000));
}

#[test]
fn image_prune_rejects_ambiguous_or_unsupported_filters() {
    for filters in [
        r#"{"dangling":[]}"#,
        r#"{"dangling":["true","false"]}"#,
        r#"{"dangling":["sometimes"]}"#,
        r#"{"reference":["team/*"]}"#,
        r#"{"until":["1","2"]}"#,
    ] {
        assert_eq!(Prune::image(Some(filters)).unwrap_err().status, StatusCode::BAD_REQUEST);
    }
    assert!(Prune::image(Some(r#"{"dangling":["false"]}"#)).is_ok());
}
