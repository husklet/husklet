use super::{
    Error,
    definition::{App, Target},
    image::TestImage,
};
use hl_container::{Config, ContainerSpec, ExitStatus, Isolation, Process, Sandbox};
use std::{fs, os::unix::fs::PermissionsExt, time::Duration};

pub enum CaseResult {
    Passed(String),
    Failed(String, String),
}

pub async fn run(app: &App, target: Target) -> Result<Vec<CaseResult>, Error> {
    let artifact = app.build(target)?;
    let fixture = TestImage::materialize(&app.image, &target.platform()).await?;
    let destination = fixture.path().join(app.destination.trim_start_matches('/'));
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&artifact, &destination)?;
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o755))?;

    let state = tempfile::tempdir()?;
    let containers = hl_container::Containers::builder(Config::new(state.path()))
        .build()
        .await?;
    let mut results = Vec::new();
    for case in &app.cases {
        let name = format!("testing-{}-{}-{}", app.name, target.name(), case.id.replace('/', "-"));
        let process = Process::new(&app.destination).args(case.arguments.iter().map(String::as_str));
        let spec = ContainerSpec::from_directory(fixture.path(), process)
            .name(&name)
            .guest(target.guest())
            .execution(app.execution.container()?)
            .isolation(Isolation {
                sandbox: Sandbox::Disabled,
                ..Isolation::default()
            });
        let outcome = async {
            containers.create(spec).await?;
            containers.start(&name).await?;
            let status = tokio::time::timeout(Duration::from_secs(case.timeout), containers.wait(&name))
                .await
                .map_err(|_| format!("timed out after {} seconds", case.timeout))??;
            let logs = containers.logs(&name).await?;
            let expected = fs::read(&case.golden)?;
            if status != ExitStatus::Code(case.exit) {
                return Err(format!("exit {status:?}, expected {}", case.exit).into());
            }
            if logs.stdout != expected {
                return Err(format!("stdout differs: got {:?}, expected {:?}", logs.stdout, expected).into());
            }
            if !logs.stderr.is_empty() {
                return Err(format!("unexpected stderr: {:?}", logs.stderr).into());
            }
            Ok::<(), Error>(())
        }
        .await;
        let cleanup = containers.remove_force(&name).await;
        let result = match (outcome, cleanup) {
            (Ok(()), Ok(_)) => CaseResult::Passed(case.id.clone()),
            (Err(error), _) => CaseResult::Failed(case.id.clone(), error.to_string()),
            (Ok(()), Err(error)) => CaseResult::Failed(case.id.clone(), format!("cleanup failed: {error}")),
        };
        results.push(result);
    }
    fixture.release()?;
    Ok(results)
}
