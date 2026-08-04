use crate::api::{Actor, Event, EventFilter, EventQuery};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;

const HISTORY: usize = 256;
const LIVE: usize = 256;

#[derive(Clone)]
pub(crate) struct Events {
    inner: Arc<EventBus>,
}

struct EventBus {
    history: Mutex<VecDeque<Event>>,
    sender: broadcast::Sender<Event>,
}

impl Events {
    pub(crate) fn new() -> Self {
        let (sender, _) = broadcast::channel(LIVE);
        Self {
            inner: Arc::new(EventBus {
                history: Mutex::new(VecDeque::with_capacity(HISTORY)),
                sender,
            }),
        }
    }

    pub(crate) fn container(&self, action: &str, container: &hl_container::Container) {
        let mut attributes = container.spec.labels.clone();
        if let Some(name) = &container.spec.name {
            attributes.insert("name".into(), name.clone());
        }
        if let Some(image) = &container.spec.image {
            attributes.insert("image".into(), image.to_string());
        }
        let duration = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        let id = container.id.to_string();
        self.publish(Event {
            kind: "container".into(),
            action: action.into(),
            actor: Actor {
                id: id.clone(),
                attributes,
            },
            scope: "local".into(),
            time: i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
            time_nano: i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX),
            status: Some(action.into()),
            id: Some(id),
            from: container.spec.image.as_ref().map(ToString::to_string),
        });
    }

    pub(crate) fn object(
        &self,
        kind: &str,
        action: &str,
        id: impl Into<String>,
        attributes: std::collections::BTreeMap<String, String>,
    ) {
        let duration = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        let id = id.into();
        self.publish(Event {
            kind: kind.into(),
            action: action.into(),
            actor: Actor {
                id: id.clone(),
                attributes,
            },
            scope: "local".into(),
            time: i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
            time_nano: i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX),
            status: None,
            id: Some(id),
            from: None,
        });
    }

    pub(crate) fn image(&self, action: &str, id: impl Into<String>, name: impl Into<String>) {
        self.object(
            "image",
            action,
            id,
            [("name".into(), name.into())].into_iter().collect(),
        );
    }

    pub(crate) fn volumes(&self, action: &str, container: &hl_container::Container) {
        for mount in &container.spec.mounts {
            let name = match &mount.source {
                hl_container::MountSource::Volume(name) | hl_container::MountSource::Anonymous(name) => name,
                hl_container::MountSource::Bind(_) | hl_container::MountSource::Tmpfs(_) => {
                    continue;
                }
            };
            self.object(
                "volume",
                action,
                name,
                [
                    ("name".into(), name.clone()),
                    ("container".into(), container.id.to_string()),
                    ("driver".into(), "local".into()),
                ]
                .into_iter()
                .collect(),
            );
        }
    }

    fn publish(&self, event: Event) {
        let mut history = self.inner.history.lock().unwrap();
        if history.len() == HISTORY {
            history.pop_front();
        }
        history.push_back(event.clone());
        let _ = self.inner.sender.send(event);
    }

    pub(crate) fn subscribe(&self, query: EventQuery) -> Subscription {
        let history = self.inner.history.lock().unwrap();
        let replay = if query.since.is_some() || query.until.is_some() {
            history.iter().filter(|event| query.matches(event)).cloned().collect()
        } else {
            VecDeque::new()
        };
        let receiver = self.inner.sender.subscribe();
        drop(history);
        Subscription {
            replay,
            receiver,
            query,
        }
    }
}

impl hl_container::LifecycleEvents for Events {
    fn emit(&self, event: hl_container::LifecycleEvent) {
        let action = match event.action {
            hl_container::LifecycleAction::Create => "create",
            hl_container::LifecycleAction::Start => "start",
            hl_container::LifecycleAction::Pause => "pause",
            hl_container::LifecycleAction::Unpause => "unpause",
            hl_container::LifecycleAction::Die => "die",
            hl_container::LifecycleAction::Restart => "restart",
            hl_container::LifecycleAction::Destroy => "destroy",
            hl_container::LifecycleAction::Oom => "oom",
            hl_container::LifecycleAction::HealthStatus => match event.container.health.as_ref() {
                Some(health) => match health.status {
                    hl_container::HealthStatus::Starting => "health_status: starting",
                    hl_container::HealthStatus::Healthy => "health_status: healthy",
                    hl_container::HealthStatus::Unhealthy => "health_status: unhealthy",
                },
                None => "health_status: starting",
            },
        };
        self.container(action, &event.container);
    }
}

pub(crate) struct Subscription {
    replay: VecDeque<Event>,
    receiver: broadcast::Receiver<Event>,
    query: EventQuery,
}

impl Subscription {
    pub(crate) async fn next(&mut self) -> Option<Event> {
        loop {
            if let Some(event) = self.replay.pop_front() {
                return Some(event);
            }
            if self.query.expired() {
                return None;
            }
            let received = if let Some(until) = self.query.until {
                let now = EventQuery::now();
                let delay = std::time::Duration::from_secs(u64::try_from(until - now).ok()?);
                tokio::select! {
                    event = self.receiver.recv() => event,
                    () = tokio::time::sleep(delay) => return None,
                }
            } else {
                self.receiver.recv().await
            };
            match received {
                Ok(event) if self.query.matches(&event) => return Some(event),
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

impl EventQuery {
    fn matches(&self, event: &Event) -> bool {
        if self.since.is_some_and(|since| event.time < since) || self.until.is_some_and(|until| event.time > until) {
            return false;
        }
        self.filters.matches(event)
    }

    fn now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|value| i64::try_from(value.as_secs()).ok())
            .unwrap_or(0)
    }

    fn expired(&self) -> bool {
        self.until.is_some_and(|until| Self::now() >= until)
    }
}

impl EventFilter {
    fn matches(&self, event: &Event) -> bool {
        self.0.iter().all(|(key, values)| {
            values.is_empty()
                || values.iter().any(|value| match key.as_str() {
                    "type" => value == &event.kind,
                    "event" | "action" => value == &event.action,
                    "container" => value == &event.actor.id || event.actor.attributes.get("name") == Some(value),
                    "image" => {
                        value == &event.actor.id
                            || event.actor.attributes.get("name") == Some(value)
                            || event.actor.attributes.get("image") == Some(value)
                            || event.from.as_ref() == Some(value)
                    }
                    "network" | "volume" => {
                        key == &event.kind
                            && (value == &event.actor.id || event.actor.attributes.get("name") == Some(value))
                    }
                    "label" => event.actor.attributes.label(value),
                    _ => false,
                })
        })
    }
}

trait Labels {
    fn label(&self, filter: &str) -> bool;
}

impl Labels for std::collections::BTreeMap<String, String> {
    fn label(&self, filter: &str) -> bool {
        filter.split_once('=').map_or_else(
            || self.contains_key(filter),
            |(name, value)| self.get(name).is_some_and(|current| current == value),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: &str, action: &str, time: i64) -> Event {
        Event {
            kind: "container".into(),
            action: action.into(),
            actor: Actor {
                id: id.into(),
                attributes: [("tier".into(), "api".into())].into_iter().collect(),
            },
            scope: "local".into(),
            time,
            time_nano: time,
            status: Some(action.into()),
            id: Some(id.into()),
            from: None,
        }
    }

    #[tokio::test]
    async fn subscription_replays_then_receives_matching_live_events() {
        let events = Events::new();
        events.publish(event("one", "create", 10));
        events.publish(event("two", "create", 11));
        let query = EventQuery::default()
            .since(10)
            .filters(EventFilter::default().container("one").label("tier=api"));
        let mut subscription = events.subscribe(query);
        assert_eq!(subscription.next().await.unwrap().actor.id, "one");

        events.publish(event("two", "destroy", 12));
        events.publish(event("one", "start", 13));
        assert_eq!(subscription.next().await.unwrap().action, "start");
    }

    #[tokio::test]
    async fn time_bounds_apply_to_replay_and_end_live_waiting() {
        let events = Events::new();
        events.publish(event("old", "create", 1));
        events.publish(event("current", "create", EventQuery::now()));
        let until = EventQuery::now() + 1;
        let mut subscription = events.subscribe(EventQuery::default().since(2).until(until));
        assert_eq!(subscription.next().await.unwrap().actor.id, "current");
        assert!(subscription.next().await.is_none());
    }

    #[test]
    fn replay_history_is_bounded() {
        let events = Events::new();
        for index in 0..=HISTORY {
            events.publish(event(&index.to_string(), "create", i64::try_from(index).unwrap()));
        }
        let subscription = events.subscribe(EventQuery::default().since(0));
        assert_eq!(subscription.replay.len(), HISTORY);
        assert_eq!(subscription.replay.front().unwrap().actor.id, "1");
    }

    #[test]
    fn docker_object_filters_match_actor_identity_and_attributes() {
        let mut value = event("sha256:abc", "tag", 1);
        value.kind = "image".into();
        value.from = Some("alpine:latest".into());
        value.actor.attributes.insert("name".into(), "frontend".into());
        value.actor.attributes.insert("image".into(), "alpine:latest".into());

        assert!(EventFilter::default().image("alpine:latest").matches(&value));
        assert!(!EventFilter::default().image("debian").matches(&value));

        value.kind = "network".into();
        assert!(EventFilter::default().network("frontend").matches(&value));
        assert!(!EventFilter::default().volume("frontend").matches(&value));
        value.kind = "volume".into();
        assert!(EventFilter::default().volume("frontend").matches(&value));
    }

    #[test]
    fn absent_and_empty_filters_match_every_event() {
        let value = event("container-one", "create", 1);
        assert!(EventFilter::default().matches(&value));
        assert!(EventFilter([("event".into(), Vec::new())].into()).matches(&value));
    }

    #[test]
    fn action_filter_matches_health_status_action() {
        let value = event("container-one", "health_status: healthy", 1);
        assert!(EventFilter::default().action("health_status: healthy").matches(&value));
        assert!(
            !EventFilter::default()
                .action("health_status: unhealthy")
                .matches(&value)
        );
    }

    #[tokio::test]
    async fn pause_identity() {
        let events = Events::new();
        let id = "00000000000040008000000000000001";
        let mut container = hl_container::Container::new(
            id.parse().unwrap(),
            hl_container::ContainerSpec::from_directory(".", hl_container::Process::new("/bin/true"))
                .name("workload"),
            hl_container::ContainerState::Created,
            1,
        );
        let mut subscription = events.subscribe(EventQuery::default());

        events.container("pause", &container);
        container.state = hl_container::ContainerState::Paused {
            process_id: 42,
            started_at_ms: 1,
            paused_at_ms: 2,
        };
        events.container("unpause", &container);

        for action in ["pause", "unpause"] {
            let event = subscription.next().await.unwrap();
            assert_eq!(event.action, action);
            assert_eq!(event.status.as_deref(), Some(action));
            assert_eq!(event.actor.id, id);
            assert_eq!(event.id.as_deref(), Some(id));
            assert_eq!(event.actor.attributes.get("name").map(String::as_str), Some("workload"));
        }
    }

    #[test]
    fn label_and_object_scope_filters_are_conjunctive() {
        let mut value = event("network-id", "connect", 1);
        value.kind = "network".into();
        value.actor.attributes.insert("name".into(), "frontend".into());

        assert!(
            EventFilter::default()
                .network("frontend")
                .label("tier=api")
                .matches(&value)
        );
        assert!(
            !EventFilter::default()
                .volume("frontend")
                .label("tier=api")
                .matches(&value)
        );
        assert!(
            !EventFilter::default()
                .network("frontend")
                .label("tier=worker")
                .matches(&value)
        );
    }

    #[test]
    fn subscription_without_time_bounds_starts_with_live_events() {
        let events = Events::new();
        events.publish(event("before-subscribe", "create", 1));

        let mut subscription = events.subscribe(EventQuery::default());
        assert!(subscription.replay.is_empty());

        events.publish(event("after-subscribe", "start", 2));
        assert_eq!(subscription.receiver.try_recv().unwrap().actor.id, "after-subscribe");
    }

    #[test]
    fn replay_to_live_handoff_is_ordered_without_duplicates() {
        let events = Events::new();
        events.publish(event("one", "create", 1));
        let mut subscription = events.subscribe(EventQuery::default().since(0));
        events.publish(event("two", "start", 2));

        assert_eq!(subscription.replay.pop_front().unwrap().actor.id, "one");
        assert!(subscription.replay.is_empty());
        assert_eq!(subscription.receiver.try_recv().unwrap().actor.id, "two");
    }
}
