use std::fs;

use crate::{
    Result,
    model::{Finding, Location, Severity},
    rule::Rule,
    source::Workspace,
};

/// Rejects source comments that explicitly mark instrumentation as provisional.
pub struct ProvisionalDiagnostic;

impl Rule for ProvisionalDiagnostic {
    fn id(&self) -> &'static str {
        "provisional-diagnostic"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        for path in workspace.source_files()? {
            let text = fs::read_to_string(&path).map_err(|error| crate::LintError::io("read", &path, error))?;
            for (index, line) in text.lines().enumerate() {
                let comment = line.trim_start();
                if !matches!(comment.get(..2), Some("//" | "/*" | "* ")) {
                    continue;
                }
                let normalized = comment.to_ascii_lowercase();
                let Some(temporary) = normalized.find("temporary") else {
                    continue;
                };
                if !normalized.contains("diagnostic") {
                    continue;
                }
                let mut finding = Finding::error(
                    self.id(),
                    "temporary diagnostics",
                    Location {
                        path: path.clone(),
                        line: index + 1,
                        column: temporary + 1,
                        source: line.to_owned(),
                    },
                );
                finding.message = "source comment declares temporary diagnostics".into();
                finding.help = "remove the diagnostics or give the observability a permanent, bounded contract".into();
                findings.push(finding);
            }
        }
        Ok(findings)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use crate::{rule::Rule, source::Workspace};

    use super::ProvisionalDiagnostic;

    fn fixture() -> PathBuf {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("hl-lint-provisional-{}-{nonce}", std::process::id()));
        fs::create_dir_all(root.join("src")).unwrap();
        root
    }

    #[test]
    fn rejects_temporary_diagnostics_in_rust_and_c_comments() {
        let root = fixture();
        fs::write(
            root.join("src/runtime.rs"),
            "// Temporary child diagnostics.\nfn run() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("src/runtime.c"),
            "/* fault diagnostics are temporary */\nvoid run(void) {}\n",
        )
        .unwrap();
        let workspace = Workspace::load([root.clone()]).unwrap();

        let findings = ProvisionalDiagnostic.check(&workspace).unwrap();

        assert_eq!(findings.len(), 2);
        assert!(findings.iter().all(|finding| finding.rule == "provisional-diagnostic"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ordinary_temporary_state_and_permanent_diagnostics_are_allowed() {
        let root = fixture();
        fs::write(
            root.join("src/runtime.rs"),
            "// Temporary directory owned by this call.\n// Bounded lifecycle diagnostics.\nconst TEXT: &str = \"temporary diagnostics\";\n",
        )
        .unwrap();
        let workspace = Workspace::load([root.clone()]).unwrap();

        assert!(ProvisionalDiagnostic.check(&workspace).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
