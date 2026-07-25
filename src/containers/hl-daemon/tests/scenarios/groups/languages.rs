use super::{contract::Group, runner::Runner};
use hl_container::{Config, Containers};
use std::path::Path;
use tempfile::TempDir;

type Error = Box<dyn std::error::Error>;

pub(crate) fn group() -> Group {
    let mut scenarios = Vec::new();
    for manifest in [
        include_str!("../fixtures/languages-dotnet.yaml"),
        include_str!("../fixtures/languages-python.yaml"),
        include_str!("../fixtures/languages-node.yaml"),
        include_str!("../fixtures/languages-ruby.yaml"),
        include_str!("../fixtures/languages-php.yaml"),
        include_str!("../fixtures/languages-perl.yaml"),
        include_str!("../fixtures/languages-elixir.yaml"),
        include_str!("../fixtures/languages-compiled.yaml"),
    ] {
        scenarios.extend(
            crate::manifest::load(manifest)
                .expect("the checked-in language manifest must satisfy the schema"),
        );
    }
    Group::new("languages", scenarios)
}

pub(crate) async fn run(work: &Path) -> Result<(), Error> {
    let state = TempDir::new_in(work)?;
    let containers = Containers::builder(Config::new(state.path().join("state")))
        .build()
        .await?;
    Runner::from_env(&containers)?.run(group()).await
}

pub(crate) mod tests {
    pub(crate) fn registry_has_every_stable_id_once() {
        let group = super::group();
        let ids = group
            .scenarios
            .iter()
            .map(|case| case.id)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(group.scenarios.len(), 55);
        assert_eq!(ids.len(), 55);
    }
}
