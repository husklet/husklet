use super::{Fields, ImageSelection, ListQuery, Prune, RemoveQuery};
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
fn system_prune_preserves_all_and_filtered_operations() {
    assert!(matches!(Prune::parse(None).unwrap(), Prune::All));
    assert!(matches!(
        Prune::parse(Some(r#"{"until":["2"],"label":["stage=build"]}"#)).unwrap(),
        Prune::Selected {
            until: Some(2_000),
            ..
        }
    ));
    assert!(Prune::parse(Some(r#"{"until":["1","2"]}"#)).is_err());
}
