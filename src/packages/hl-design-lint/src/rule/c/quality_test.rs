use super::{has_call, inventory_findings, sanitize};
use std::fs;

#[test]
fn comments_and_strings_do_not_create_calls() {
    let mut block = false;
    assert!(!has_call(&sanitize("// getenv(\"X\")", &mut block), "getenv"));
    assert!(!has_call(
        &sanitize("const char *x = \"system(x)\";", &mut block),
        "system"
    ));
    assert!(has_call(&sanitize("return getenv (name);", &mut block), "getenv"));
}

#[test]
fn inventory_detects_omissions_and_stale_rows() {
    let root = std::env::temp_dir().join(format!("hl-native-rule-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("lint-sources.manifest"), "source\tstale.c\n").unwrap();
    fs::write(root.join("sub/new.c"), "int x;\n").unwrap();
    let findings = inventory_findings(&root, &[root.join("sub/new.c")]).unwrap();
    assert_eq!(findings.len(), 2);
    fs::remove_dir_all(root).unwrap();
}
