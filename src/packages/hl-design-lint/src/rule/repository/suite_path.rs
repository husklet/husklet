use std::path::Path;

use crate::{
    Result,
    model::{Finding, Location, Severity},
    rule::Rule,
    source::Workspace,
};

/// Requires test suite root directories to use kebab-case names.
pub struct KebabPath;

impl Rule for KebabPath {
    fn id(&self) -> &'static str {
        "test-suite-kebab-path"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for root in workspace.paths() {
            visit(root, self.id(), &mut findings)?;
        }
        findings.sort_by(|left, right| left.location.path.cmp(&right.location.path));
        findings.dedup_by(|left, right| left.location.path == right.location.path);
        Ok(findings)
    }
}

fn visit(path: &Path, rule: &'static str, findings: &mut Vec<Finding>) -> Result<()> {
    if !path.is_dir() {
        return Ok(());
    }
    if is_runtime_suite(path)
        && path.join("test.yaml").is_file()
        && let Some(name) = path.file_name().and_then(|name| name.to_str())
        && name.contains('_')
    {
        let suggestion = name.replace('_', "-");
        let mut finding = Finding::error(
            rule,
            name,
            Location {
                path: path.to_owned(),
                line: 1,
                column: 1,
                source: String::new(),
            },
        );
        finding.message = format!("test suite directory `{name}` is not kebab-case");
        finding.help = format!("rename the suite directory to `{suggestion}` and update every manifest reference");
        findings.push(finding);
    }
    let entries = std::fs::read_dir(path).map_err(|error| crate::LintError::io("walk", path, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| crate::LintError::io("walk", path, error))?;
        let child = entry.path();
        if child.is_dir()
            && child
                .file_name()
                .and_then(|name| name.to_str())
                .is_none_or(|name| !matches!(name, ".git" | "target" | "vendor" | "lint"))
        {
            visit(&child, rule, findings)?;
        }
    }
    Ok(())
}

fn is_runtime_suite(path: &Path) -> bool {
    let components: Vec<_> = path.components().collect();
    components
        .windows(2)
        .any(|pair| pair[0].as_os_str() == "tests" && pair[1].as_os_str() == "runtime")
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::KebabPath;
    use crate::{rule::Rule, source::Workspace};

    fn fixture(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "hl-design-lint-suite-path-{name}-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn rejects_underscore_suite_and_suggests_kebab_case() {
        let root = fixture("reject");
        let suite = root.join("tests/runtime/abi_core");
        std::fs::create_dir_all(&suite).unwrap();
        std::fs::write(suite.join("test.yaml"), "cases: []\n").unwrap();
        let findings = KebabPath.check(&Workspace::load([root.clone()]).unwrap()).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].subject, "abi_core");
        assert!(findings[0].help.contains("abi-core"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn accepts_kebab_suite_and_nested_architecture_directory() {
        let root = fixture("accept");
        let suite = root.join("tests/runtime/abi-core");
        std::fs::create_dir_all(suite.join("golden/x86_64")).unwrap();
        std::fs::write(suite.join("test.yaml"), "cases: []\n").unwrap();
        std::fs::write(suite.join("golden/x86_64/output.txt"), "ok\n").unwrap();
        assert!(
            KebabPath
                .check(&Workspace::load([root.clone()]).unwrap())
                .unwrap()
                .is_empty()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ignores_benchmark_suite_names() {
        let root = fixture("bench");
        let suite = root.join("tests/bench/file_io");
        std::fs::create_dir_all(&suite).unwrap();
        std::fs::write(suite.join("test.yaml"), "cases: []\n").unwrap();
        assert!(
            KebabPath
                .check(&Workspace::load([root.clone()]).unwrap())
                .unwrap()
                .is_empty()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
