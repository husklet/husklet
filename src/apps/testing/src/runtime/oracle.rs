use super::*;

pub fn oracle(options: OracleOptions) -> Result<(), Error> {
    let apps = apps(&options.runtime)?;
    validate_case_ids(&apps)?;
    if let Some(selected) = options.runtime.selection.case.as_deref()
        && !apps.iter().any(|app| app.cases.iter().any(|case| case.id == selected))
    {
        return Err(format!("no runtime case exactly matched --case {selected}").into());
    }
    let mut eligible = false;
    for app in apps {
        for target in options.runtime.selection.targets() {
            if !app.supports(target) {
                continue;
            }
            let mut cases = app.cases_for(target);
            if !cases.any(|case| {
                options
                    .runtime
                    .selection
                    .case
                    .as_deref()
                    .is_none_or(|selected| case.id == selected)
            }) {
                continue;
            }
            eligible = true;
            app.oracle(target, options.update, options.runtime.selection.case.as_deref())?;
        }
    }
    if eligible {
        Ok(())
    } else {
        Err("no oracle cases support the selected target(s)".into())
    }
}

fn validate_case_ids(apps: &[App]) -> Result<(), Error> {
    let mut ids = std::collections::BTreeSet::new();
    for case in apps.iter().flat_map(|app| &app.cases) {
        if !ids.insert(&case.id) {
            return Err(format!("runtime case ID is duplicated: {}", case.id).into());
        }
    }
    Ok(())
}

fn apps(options: &Options) -> Result<Vec<App>, Error> {
    let root = workspace()?.join("tests/runtime");
    let mut directories = std::fs::read_dir(&root)?
        .map(|entry| entry.map(|value| value.path()))
        .collect::<Result<Vec<_>, _>>()?;
    directories.sort();
    let mut result = Vec::new();
    for directory in directories.into_iter().filter(|value| value.is_dir()) {
        let name = directory
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if options.app.as_deref().is_some_and(|selected| selected != name) {
            continue;
        }
        let definition = directory.join("test.yaml");
        if definition.is_file() {
            result.push(App::load(&directory, &definition)?);
        }
    }
    if result.is_empty() {
        return Err(format!("no runtime apps matched under {}", root.display()).into());
    }
    Ok(result)
}
#[derive(Args)]
pub(crate) struct OracleOptions {
    /// Replace checked golden output with oracle output.
    #[arg(long, conflicts_with = "check")]
    update: bool,
    /// Check oracle output against the golden (the default).
    #[arg(long)]
    check: bool,
    #[command(flatten)]
    runtime: Options,
}
