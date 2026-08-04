use super::{Fields, ImageSelection, ListQuery, Prune, RemoveQuery, removal_conflicts};
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
