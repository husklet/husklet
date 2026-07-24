use std::fmt;

use hl_container::ContainerState;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum State {
    Created,
    Running,
    Paused,
    Restarting,
    Exited,
}

impl From<&ContainerState> for State {
    fn from(state: &ContainerState) -> Self {
        match state {
            ContainerState::Created => Self::Created,
            ContainerState::Running { .. } => Self::Running,
            ContainerState::Paused { .. } => Self::Paused,
            ContainerState::Restarting { .. } => Self::Restarting,
            ContainerState::Exited { .. } => Self::Exited,
        }
    }
}

impl fmt::Display for State {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Created => "created",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Restarting => "restarting",
            Self::Exited => "exited",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::State;

    #[test]
    fn state_uses_docker_lifecycle_spellings() {
        assert_eq!(State::Created.to_string(), "created");
        assert_eq!(State::Running.to_string(), "running");
        assert_eq!(State::Paused.to_string(), "paused");
        assert_eq!(State::Restarting.to_string(), "restarting");
        assert_eq!(State::Exited.to_string(), "exited");
    }
}
