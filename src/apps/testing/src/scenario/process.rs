use super::{
    Error,
    definition::{Resource, ScenarioAction, ScenarioCase},
};
use hl_container::{Console, Process, Size};

pub(super) fn for_case(case: &ScenarioCase) -> Result<Process, Error> {
    let mut process = match &case.actions[0] {
        ScenarioAction::Argv(argv) => {
            let (program, arguments) = argv.split_first().ok_or("argv action is empty")?;
            Process::new(program).args(arguments.iter().map(String::as_str))
        }
        ScenarioAction::Shell(script) => Process::new("/bin/sh").args(["-c", script]),
        ScenarioAction::Entrypoint => {
            return Err(format!("{} entrypoint execution requires image runtime metadata", case.id).into());
        }
        ScenarioAction::Host(script) => {
            return Err(format!(
                "{} host action requires a typed host adapter (script_bytes={})",
                case.id,
                script.len()
            )
            .into());
        }
        ScenarioAction::Api(operation) => {
            return Err(format!(
                "{} API action requires a typed daemon adapter (operation={operation:?})",
                case.id
            )
            .into());
        }
    }
    .working_dir(&case.working_directory);
    for (name, value) in &case.environment {
        process = process.env(name, value);
    }
    if case.resources.contains(&Resource::Pty) {
        if !case.environment.contains_key("TERM") {
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
    use super::for_case;
    use crate::{
        scenario::definition::{Class, Resource, ScenarioAction, ScenarioCase},
        suite::{Execution, Target},
    };
    use hl_container::{Console, Size};
    use std::collections::BTreeMap;

    fn scenario(resources: Vec<Resource>, environment: BTreeMap<String, String>) -> ScenarioCase {
        ScenarioCase {
            id: "terminal/probe".to_owned(),
            image: "alpine".to_owned(),
            execution: Execution::default(),
            class: Class::Quick,
            targets: vec![Target::Arm64, Target::Amd64],
            expected_failures: Vec::new(),
            resources,
            environment,
            working_directory: "/".to_owned(),
            actions: vec![ScenarioAction::Shell("true".to_owned())],
            fixtures: Vec::new(),
            readiness: None,
            timeout: 60,
            exit: 0,
            stdout_contains: Vec::new(),
            stdout_exact: None,
        }
    }

    #[test]
    fn pty_has_bounded_console_and_term_defaults() {
        let terminal = for_case(&scenario(vec![Resource::Pty], BTreeMap::new())).unwrap();
        assert_eq!(
            terminal.console,
            Console {
                stdin: false,
                terminal: Some(Size::default()),
            }
        );
        assert_eq!(terminal.console.terminal.unwrap().rows(), 24);
        assert_eq!(terminal.console.terminal.unwrap().columns(), 80);
        assert_eq!(terminal.env.get("TERM").map(String::as_str), Some("xterm"));

        let explicit = for_case(&scenario(
            vec![Resource::Pty],
            BTreeMap::from([("TERM".to_owned(), "screen".to_owned())]),
        ))
        .unwrap();
        assert_eq!(explicit.env.get("TERM").map(String::as_str), Some("screen"));

        let plain = for_case(&scenario(Vec::new(), BTreeMap::new())).unwrap();
        assert_eq!(plain.console, Console::default());
        assert!(!plain.env.contains_key("TERM"));
    }
}
