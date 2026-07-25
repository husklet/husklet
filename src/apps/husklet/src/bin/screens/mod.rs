pub mod home;
pub mod workspace;

/// Stable product routes. Widgets may change without changing navigation intent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Screen {
    Home,
    Workspace,
    WorkspaceCreate,
}
