use super::Process;
use crate::benchmark::{Provider, Run};
use std::path::Path;
use std::process::{Command, Stdio};

impl Process {
    pub(in crate::benchmark) fn command(&self, run: &Run) -> Result<Command, String> {
        let host_binary = run.host_binary();
        if !host_binary.is_file() {
            return Err(format!("guest does not exist: {}", host_binary.display()));
        }
        let mut command = match run.provider {
            Provider::Native => Command::new(&host_binary),
            Provider::Qemu => {
                let mut command = Command::new(format!("qemu-{}", run.isa.name()));
                if let Some(rootfs) = &run.rootfs {
                    command.args(["-L".as_ref(), rootfs.as_os_str()]);
                }
                command.arg(&host_binary);
                command
            }
            Provider::C => {
                let engine = run.engine.as_ref().expect("validated engine");
                if run.c_runner.is_none() && Process::is_c_exec_wrapper(engine)? {
                    return Err("C exec wrapper configured as --engine; pass it as --c-runner and provide the retained engine separately".into());
                }
                let mut command = if let Some(runner) = &run.c_runner {
                    let mut command = Command::new(runner);
                    command.arg(engine);
                    command
                } else {
                    Command::new(engine)
                };
                if let Some(rootfs) = &run.rootfs {
                    command.arg("--rootfs").arg(rootfs);
                }
                command.arg(&run.binary);
                command
            }
            Provider::Rust => {
                let mut command = Command::new(run.engine.as_ref().expect("validated engine"));
                command.args(["--guest-isa", run.isa.name()]);
                for (name, value) in &run.engine_options {
                    command.args(["--engine-option", &format!("{name}={value}")]);
                }
                if let Some(rootfs) = &run.rootfs {
                    command.arg("--rootfs").arg(rootfs);
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

    fn is_c_exec_wrapper(path: &Path) -> Result<bool, String> {
        const SIGNATURE: &[u8] = b"ENGINE GUEST [args...]";
        let bytes = std::fs::read(path).map_err(|error| format!("read C engine capability: {error}"))?;
        Ok(bytes
            .windows(SIGNATURE.len())
            .any(|window| window == SIGNATURE))
    }

    pub(in crate::benchmark) fn available(&self, name: &str) -> bool {
        self.search_path
            .as_deref()
            .is_some_and(|paths| std::env::split_paths(paths).any(|path| Self::executable(&path.join(name))))
    }
}
