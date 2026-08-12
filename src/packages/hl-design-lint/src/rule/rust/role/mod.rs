use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use crate::{
    Result,
    model::{Finding, Location, Review, Severity},
    rule::Rule,
    source::{Source, Workspace},
};

#[cfg(test)]
#[path = "test.rs"]
mod tests;

/// Rejects sibling files organized primarily by a repeated implementation role.
pub struct Suffix;

impl Rule for Suffix {
    fn id(&self) -> &'static str {
        "flat-role-density"
    }

    fn severity(&self) -> Severity {
        Severity::Error
    }

    fn check(&self, workspace: &Workspace) -> Result<Vec<Finding>> {
        let mut groups = BTreeMap::<(PathBuf, String), Vec<&Source>>::new();
        for source in workspace.sources() {
            let Some(parent) = source.path.parent() else { continue };
            let Some(stem) = source.path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let words = stem.split('_').collect::<Vec<_>>();
            let Some(role) = words.last() else { continue };
            if words.len() < 2 || !implementation_role(role) {
                continue;
            }
            groups
                .entry((parent.to_owned(), (*role).to_owned()))
                .or_default()
                .push(source);
        }
        Ok(groups
            .into_iter()
            .filter(|(_, sources)| sources.len() >= 3)
            .map(|((parent, role), sources)| finding(self.id(), &parent, &role, &sources))
            .collect())
    }
}

fn finding(rule: &'static str, parent: &Path, role: &str, sources: &[&Source]) -> Finding {
    let names = sources
        .iter()
        .filter_map(|source| source.path.file_name()?.to_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut finding = Finding::error(
        rule,
        role,
        Location {
            path: sources[0].path.clone(),
            line: 1,
            column: 1,
            source: String::new(),
        },
    );
    finding.message = format!(
        "{} sibling files repeat the implementation role `_{role}`: {names}",
        sources.len()
    );
    finding.help = "organize files by the noun that owns state and behavior; keep roles behind that boundary instead of encoding a flat pseudo-layer in filenames".into();
    let mut review = Review::error();
    review.metadata = vec![
        ("directory".into(), parent.display().to_string()),
        ("role".into(), role.into()),
        ("siblings".into(), sources.len().to_string()),
    ];
    review.questions = vec![
        "Which independent nouns own these capabilities?".into(),
        "Does the repeated role conceal dependencies between their state machines?".into(),
    ];
    finding.review = Some(review);
    finding
}

fn implementation_role(word: &str) -> bool {
    matches!(
        word,
        "registry"
            | "adapter"
            | "service"
            | "handler"
            | "manager"
            | "controller"
            | "context"
            | "port"
            | "host"
            | "activity"
            | "abi"
            | "exec"
            | "exit"
    )
}
