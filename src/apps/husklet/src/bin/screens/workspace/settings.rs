/// Sections shared by workspace creation and an existing workspace's settings tab.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Section {
    Terminal,
    Resources,
    Environment,
    Mounts,
    Docker,
    Network,
}
