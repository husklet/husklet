//! Rust orchestration replacing shell-era end-to-end workflows.

mod build;
mod compose;
mod fixture;
mod network;
mod pty;

use hl_container::Containers;

type Error = Box<dyn std::error::Error>;

pub(crate) const NAMES: [&str; 5] = [
    "docker-build",
    "docker-net",
    "compose",
    "compose-multinet",
    "pty-conformance",
];

pub(crate) async fn run(name: &str, containers: &Containers) -> Result<(), Error> {
    match name {
        "docker-build" => build::run(containers).await,
        "docker-net" => network::run(containers).await,
        "compose" => compose::run(containers).await,
        "compose-multinet" => compose::multinet(containers).await,
        "pty-conformance" => pty::run(containers).await,
        known if NAMES.contains(&known) => {
            Err(format!("workflow {known:?} is inventoried but its typed orchestration is not transferred").into())
        }
        _ => Err(format!("unknown workflow {name:?}").into()),
    }
}
