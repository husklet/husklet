use super::{
    Error,
    definition::{Resource, Sample, Step},
};
use hl_container::{Console, Process, Size};
use hl_images::RuntimeConfig;

pub(super) fn initial(case: &Sample, runtime: &RuntimeConfig, rootfs: &std::path::Path) -> Result<Process, Error> {
    let action = case.actions.iter().find(|action| matches!(action, Step::Entrypoint));
    let process = match action {
        Some(Step::Entrypoint) => {
            let mut argv = runtime.entrypoint.clone();
            argv.extend(runtime.command.iter().cloned());
            argv_process(&argv)?
        }
        _ => Process::new("/bin/sh").args(["-c", "while :; do sleep 3600; done"]),
    };
    configure(process, case, runtime, rootfs)
}

pub(super) fn action(
    case: &Sample,
    action: &Step,
    runtime: &RuntimeConfig,
    rootfs: &std::path::Path,
) -> Result<Process, Error> {
    let (process, terminal) = match action {
        Step::Argv(argv) => (argv_process(argv)?, None),
        Step::Shell(script) => (Process::new("/bin/sh").args(["-c", script]), None),
        Step::Entrypoint => return Err("entrypoint is the container initial process".into()),
        Step::Host(script) => {
            return Err(format!(
                "{} host action requires a typed host adapter (script_bytes={})",
                case.id,
                script.len()
            )
            .into());
        }
        Step::Api(operation) => {
            return Err(format!(
                "{} API action requires a typed daemon adapter (operation={operation:?})",
                case.id
            )
            .into());
        }
        Step::Terminal(terminal) => (
            argv_process(&terminal.argv)?,
            Some(Size::new(terminal.rows, terminal.columns)?),
        ),
    };
    let process = configure(process, case, runtime, rootfs)?;
    Ok(match terminal {
        Some(size) => process.console(Console {
            stdin: true,
            terminal: Some(size),
        }),
        None => process,
    })
}

pub(super) fn terminal(
    case: &Sample,
    action: &super::terminal::Action,
    runtime: &RuntimeConfig,
    rootfs: &std::path::Path,
) -> Result<Process, Error> {
    let process = configure(argv_process(&action.argv)?, case, runtime, rootfs)?;
    Ok(process.console(Console {
        stdin: true,
        terminal: Some(Size::new(action.rows, action.columns)?),
    }))
}

fn argv_process(argv: &[String]) -> Result<Process, Error> {
    let (program, arguments) = argv.split_first().ok_or("argv action is empty")?;
    Ok(Process::new(program).args(arguments.iter().map(String::as_str)))
}

fn configure(
    mut process: Process,
    case: &Sample,
    runtime: &RuntimeConfig,
    rootfs: &std::path::Path,
) -> Result<Process, Error> {
    for (name, value) in &runtime.environment {
        process = process.env(name, value);
    }
    for (name, value) in &case.environment {
        process = process.env(name, value);
    }
    let working_directory = if case.working_directory == "/" && !runtime.working_directory.is_empty() {
        &runtime.working_directory
    } else {
        &case.working_directory
    };
    process = process.working_dir(working_directory);
    if !runtime.user.is_empty() {
        let (uid, gid) = Process::resolve_user(&runtime.user, rootfs)?;
        process = process.user(uid, gid);
    }
    if case.resources.contains(&Resource::Pty) {
        if !runtime.environment.contains_key("TERM") && !case.environment.contains_key("TERM") {
            process = process.env("TERM", "xterm");
        }
        process = process.console(Console {
            stdin: false,
            terminal: Some(Size::default()),
        });
    }
    Ok(process)
}

#[cfg(test)]
mod tests {
    use super::{action, initial};
    use crate::{
        scenario::definition::{Class, Sample, Step},
        suite::{Execution, Target},
    };
    use hl_images::RuntimeConfig;
    use std::collections::BTreeMap;

    fn case(actions: Vec<Step>) -> Sample {
        Sample {
            id: "process/metadata".into(),
            image: "fixture".into(),
            execution: Execution::default(),
            class: Class::Quick,
            targets: vec![Target::Arm64],
            expected_failures: Vec::new(),
            resources: Vec::new(),
            environment: BTreeMap::from([("OVERRIDE".into(), "case".into())]),
            working_directory: "/".into(),
            actions,
            fixtures: Vec::new(),
            readiness: None,
            timeout: 1,
            warmups: 0,
            repetitions: 1,
            exit: 0,
            stdout_contains: Vec::new(),
            stdout_exact: None,
            output_empty: false,
        }
    }

    fn runtime() -> RuntimeConfig {
        RuntimeConfig {
            entrypoint: vec!["/entry".into()],
            command: vec!["default".into()],
            environment: BTreeMap::from([("IMAGE".into(), "yes".into()), ("OVERRIDE".into(), "image".into())]),
            working_directory: "/image-work".into(),
            user: String::new(),
        }
    }

    #[test]
    fn entrypoint_joins_image_entrypoint_and_command() {
        let root = tempfile::tempdir().unwrap();
        let process = initial(&case(vec![Step::Entrypoint]), &runtime(), root.path()).unwrap();
        assert_eq!(process.program, "/entry");
        assert_eq!(process.args, ["default"]);
    }

    #[test]
    fn image_defaults_are_applied_and_case_environment_wins() {
        let root = tempfile::tempdir().unwrap();
        let case = case(vec![Step::Shell("true".into())]);
        let process = action(&case, &case.actions[0], &runtime(), root.path()).unwrap();
        assert_eq!(process.working_dir, std::path::Path::new("/image-work"));
        assert_eq!(process.env.get_text("IMAGE"), Some("yes"));
        assert_eq!(process.env.get_text("OVERRIDE"), Some("case"));
    }
}
