use super::Inotify;

impl Inotify {
    pub(crate) fn retire_inner(&self) {
        let (subscription, tokens) = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.retired {
                return;
            }
            state.retired = true;
            self.inner.changed.notify_all();
            let tokens = state
                .slots
                .iter_mut()
                .filter_map(|slot| slot.watch.take().map(|watch| watch.token))
                .collect::<Vec<_>>();
            (state.source_subscription.take(), tokens)
        };
        if let Some(subscription) = subscription {
            subscription.quiesce();
        }
        for token in tokens {
            let _ = self.inner.source.remove(token);
        }
        self.inner.readiness.notify();
        self.inner.readiness.close();
    }
}
