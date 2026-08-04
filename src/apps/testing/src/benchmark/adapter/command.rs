use super::Process;
use crate::benchmark::{Provider, Run};
use std::path::Path;
use std::process::{Command, Stdio};

impl Process {
    pub(in crate::benchmark) fn command(&self, run: &Run) -> Result<Command, String> {
        if !run.binary.is_file() {
            return Err(format!("guest does not exist: {}", run.binary.display()));
        }
        let mut command = match run.provider {
            Provider::Native => Command::new(&run.binary),
            Provider::Qemu => {
                let mut command = Command::new(format!("qemu-{}", run.isa.name()));
                command.arg(&run.binary);
                command
            }
            Provider::C => {
                let mut command = Command::new(run.engine.as_ref().expect("validated engine"));
                command.arg(&run.binary);
                command
            }
            Provider::Rust => {
                let mut command = Command::new(run.engine.as_ref().expect("validated engine"));
                command.args(["--guest-isa", run.isa.name()]);
                for (name, value) in &run.engine_options {
                    command.args(["--engine-option", &format!("{name}={value}")]);
                }
                command.arg(&run.binary);
                command
            }
        };
        command
            .args(&run.guest)
            .envs(run.environment.iter().cloned())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        Ok(command)
    }

    pub(in crate::benchmark) fn executable(path: &Path) -> bool {
        path.is_file()
    }

    pub(in crate::benchmark) fn available(&self, name: &str) -> bool {
        self.search_path
            .as_deref()
            .is_some_and(|paths| std::env::split_paths(paths).any(|path| Self::executable(&path.join(name))))
    }
}
