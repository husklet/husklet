use std::collections::BTreeMap;
use std::sync::Arc;

use crate::engine::EngineExit;

use super::threads;

#[derive(Default)]
pub(super) struct State {
    pub(super) exits: BTreeMap<usize, EngineExit>,
    pub(super) running: BTreeMap<usize, Arc<threads::ThreadSet>>,
}
