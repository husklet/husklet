use serde::{Deserialize, Serialize};

/// Portable Linux signal subset accepted by container lifecycle operations.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    #[default]
    Terminate,
    Kill,
    Interrupt,
    Quit,
    Hangup,
    User1,
    User2,
}

/// State transition observed by a waiter.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WaitCondition {
    /// Return when the process is no longer running (or was already exited).
    #[default]
    NotRunning,
    /// Return after the next process generation exits, even if it will restart.
    NextExit,
    /// Return only after the container metadata has been removed.
    Removed,
}

/// Durable ownership policy applied after terminal process completion.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalPolicy {
    #[default]
    Retain,
    Automatic,
}
