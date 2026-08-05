use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use hl_descriptor::DescriptionIdentity;
use hl_event::Epoll;

use crate::{Control, GraphSnapshot};

pub(crate) struct PreparedEpollExec {
    control: Arc<Control>,
    retired: BTreeSet<DescriptionIdentity>,
    graph: Option<GraphSnapshot>,
    epolls: BTreeMap<DescriptionIdentity, Arc<Epoll>>,
    published: bool,
}

impl Control {
    pub(crate) fn prepare_exec(self: &Arc<Self>, retired: BTreeSet<DescriptionIdentity>) -> PreparedEpollExec {
        PreparedEpollExec {
            control: self.clone(),
            retired,
            graph: None,
            epolls: BTreeMap::new(),
            published: false,
        }
    }
}

impl PreparedEpollExec {
    pub(crate) fn publish(&mut self) -> bool {
        if self.published {
            return false;
        }
        let _mutation = self
            .control
            .mutation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.graph = Some(self.control.graph.snapshot());
        for identity in &self.retired {
            self.control.graph.close(*identity);
        }
        let mut state = self
            .control
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for identity in &self.retired {
            if let Some(epoll) = state.epolls.remove(identity) {
                self.epolls.insert(*identity, epoll);
            }
        }
        self.published = true;
        true
    }

    pub(crate) fn rollback(&mut self) {
        if !self.published {
            return;
        }
        let _mutation = self
            .control
            .mutation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(graph) = self.graph.take() {
            self.control.graph.restore(&graph);
        }
        self.control
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .epolls
            .append(&mut self.epolls);
        self.published = false;
    }

    pub(crate) fn finish(&mut self) {
        self.graph = None;
        for epoll in self.epolls.values() {
            epoll.finish_retirement()
        }
        self.epolls.clear();
        self.published = false;
    }
}
