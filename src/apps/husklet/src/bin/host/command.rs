use crate::*;

pub(crate) struct Hl;

impl Hl {
    pub(crate) fn command(args: &[&str]) -> std::process::Command {
        let mut c = std::process::Command::new(application_path());
        c.arg("--worker").args(args);
        AppConfig::get().environment.apply(&mut c);
        c
    }
}

pub(crate) fn application_path() -> std::path::PathBuf {
    std::env::current_exe().unwrap_or_else(|_| "husklet".into())
}
