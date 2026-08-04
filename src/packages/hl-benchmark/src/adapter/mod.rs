use crate::{Provider, Run};
use std::path::Path;
use std::process::{Command, Stdio};

mod process;

pub(super) use process::sample;

#[hl_design::classify(pkg)]
pub(super) fn command(run: &Run) -> Result<Command, String> {
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

#[hl_design::classify(pkg)]
pub(super) fn executable(path: &Path) -> bool {
    path.is_file()
}

#[hl_design::classify(pkg)]
pub(super) fn available(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|path| executable(&path.join(name)))
}
