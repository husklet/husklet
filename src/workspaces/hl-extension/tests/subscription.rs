//! Routing, revocation, and backpressure for host-pushed state and bulk bytes.
//!
//! Driven entirely by in-memory ports, like the rest of the suite: a session is
//! subscribed through the real dispatch path so that what is tested here is the
//! same grant check the protocol performs, not a restatement of it.

use hl_extension::port::{
    ContainerControl, ContainerInventory, ContainerSummary, Division, Entry, HostError, ImageStore, ImageSummary,
    PaneText, TabSummary, TerminalSurface, WorkspaceFiles, WorkspaceInventory, WorkspaceState,
};
use hl_extension::{
    Authority, Capability, Channels, Emission, ExtensionName, Grant, Outbox, Parcel, RelativePath, Request, Services,
    Session, Snapshot, Streams, Subscriptions, Topic, WorkspaceInfo,
};

/// The narrowest host that satisfies the ports. Nothing here is exercised: the
/// subject is routing, and the services exist only so a subscribe can be made
/// through `dispatch` rather than around it.
struct Host;

impl ContainerInventory for Host {
    fn list(&self) -> Result<Vec<ContainerSummary>, HostError> {
        Ok(Vec::new())
    }

    fn inspect(&self, id: &str) -> Result<ContainerSummary, HostError> {
        Err(HostError::Absent(id.into()))
    }
}

impl ContainerControl for Host {
    fn create(&self, _image: &str, _name: &str) -> Result<String, HostError> {
        Ok("c1".into())
    }

    fn start(&self, _id: &str) -> Result<(), HostError> {
        Ok(())
    }

    fn stop(&self, _id: &str) -> Result<(), HostError> {
        Ok(())
    }

    fn remove(&self, _id: &str) -> Result<(), HostError> {
        Ok(())
    }
}

impl ImageStore for Host {
    fn list(&self) -> Result<Vec<ImageSummary>, HostError> {
        Ok(Vec::new())
    }

    fn pull(&self, _reference: &str) -> Result<ImageSummary, HostError> {
        Err(HostError::Failed("no registry".into()))
    }
}

impl TerminalSurface for Host {
    fn tabs(&self) -> Result<Vec<TabSummary>, HostError> {
        Ok(Vec::new())
    }

    fn open_tab(&self, title: &str) -> Result<String, HostError> {
        Ok(format!("tab-{title}"))
    }

    fn split(&self, _slot: &str, _division: Division) -> Result<String, HostError> {
        Ok("s2".into())
    }

    fn spawn(&self, _slot: &str, _command: &[String]) -> Result<(), HostError> {
        Ok(())
    }
    fn read(&self, slot: &str, lines: usize) -> Result<PaneText, HostError> {
        Ok(PaneText {
            slot: slot.into(),
            lines: vec![format!("at most {lines}")],
            truncated: true,
        })
    }

    fn close(&self, _slot: &str) -> Result<(), HostError> {
        Ok(())
    }

    fn focus(&self, _slot: &str) -> Result<(), HostError> {
        Ok(())
    }

    fn ratio(&self, _slot: &str, _ratio: f64) -> Result<(), HostError> {
        Ok(())
    }

    fn surface(&self, _slot: &str, _division: Division) -> Result<String, HostError> {
        Ok("s3".into())
    }
}

impl WorkspaceInventory for Host {
    fn workspaces(&self) -> Result<Vec<WorkspaceState>, HostError> {
        Ok(vec![WorkspaceState {
            name: "dev".into(),
            architecture: "arm64".into(),
            image: "alpine:3.20".into(),
            running: true,
            current: true,
        }])
    }
}

impl WorkspaceFiles for Host {
    fn list(&self, _path: &RelativePath) -> Result<Vec<Entry>, HostError> {
        Ok(Vec::new())
    }

    fn read(&self, _path: &RelativePath) -> Result<Vec<u8>, HostError> {
        Ok(Vec::new())
    }

    fn write(&self, _path: &RelativePath, _contents: &[u8]) -> Result<(), HostError> {
        Ok(())
    }
}

fn services(host: &Host) -> Services<'_> {
    Services {
        workspace: WorkspaceInfo {
            name: "dev".into(),
            architecture: "arm64".into(),
            image: "alpine:3.20".into(),
        },
        workspaces: host,
        containers: host,
        control: host,
        images: host,
        terminal: host,
        files: host,
    }
}

fn session(capabilities: &[Capability]) -> Session {
    Session::new(Authority::new(
        ExtensionName::new("sample").expect("name"),
        Grant::new(capabilities.iter().copied()),
        Vec::new(),
    ))
}

/// Subscribes through dispatch and allocates the topic's channel, the way a
/// host handles `EventSubscribe`.
fn follow(session: &mut Session, topic: Topic, subscriptions: &mut Subscriptions, channels: &mut Channels) {
    session
        .dispatch(&Request::EventSubscribe { topic }, &services(&Host))
        .expect("subscribed");
    subscriptions.open(topic, channels).expect("channel");
}

/// A listing whose length identifies which emission it came from, so the
/// survivor of a coalescing queue can be told apart from the ones it replaced.
fn containers(count: usize) -> Snapshot {
    let listing = (0..count)
        .map(|index| ContainerSummary {
            id: format!("c{index}"),
            name: format!("service-{index}"),
            image: "husklet/api:1".into(),
            state: "running".into(),
            created: 0,
        })
        .collect();
    Snapshot::Containers(listing)
}

fn payload(snapshot: &Snapshot) -> Vec<u8> {
    snapshot.payload().expect("encoded")
}

fn decode(bytes: &[u8]) -> Snapshot {
    serde_json::from_slice(bytes).expect("a listing")
}

#[test]
fn a_topic_the_session_never_followed_is_not_queued() {
    let session = session(&[Capability::ContainerRead]);
    let mut channels = Channels::new();
    let mut outbox = Outbox::new();
    let mut subscriptions = Subscriptions::new();
    subscriptions.open(Topic::Containers, &mut channels).expect("channel");

    let emission = subscriptions.emit(
        Topic::Containers,
        payload(&containers(1)),
        &session,
        &mut channels,
        &mut outbox,
    );

    assert_eq!(
        emission,
        Emission::Ignored,
        "a channel existing is not the same as the extension asking for the topic"
    );
    assert!(outbox.is_empty());
}

#[test]
fn revoking_a_capability_stops_an_established_subscription_mid_flight() {
    let mut session = session(&[Capability::ContainerRead]);
    let mut channels = Channels::new();
    let mut outbox = Outbox::new();
    let mut subscriptions = Subscriptions::new();
    follow(&mut session, Topic::Containers, &mut subscriptions, &mut channels);

    let first = subscriptions.emit(
        Topic::Containers,
        payload(&containers(1)),
        &session,
        &mut channels,
        &mut outbox,
    );
    assert_eq!(first, Emission::Queued);
    let queued = outbox.len();

    session.authority_mut().revoke(Capability::ContainerRead);
    let after = subscriptions.emit(
        Topic::Containers,
        payload(&containers(2)),
        &session,
        &mut channels,
        &mut outbox,
    );

    assert_eq!(
        after,
        Emission::Ignored,
        "the grant is re-checked at emission, so a revoked one stops a stream already running"
    );
    assert_eq!(outbox.len(), queued, "and nothing more is queued");
}

#[test]
fn a_subscriber_that_never_returns_credit_cannot_grow_the_host() {
    let mut session = session(&[Capability::ContainerRead]);
    let mut channels = Channels::new();
    let mut outbox = Outbox::new();
    let mut subscriptions = Subscriptions::new();
    follow(&mut session, Topic::Containers, &mut subscriptions, &mut channels);
    let channel = subscriptions.channel(Topic::Containers).expect("routed");

    let last = 2_000;
    for count in 1..=last {
        subscriptions.emit(
            Topic::Containers,
            payload(&containers(count)),
            &session,
            &mut channels,
            &mut outbox,
        );
    }

    assert!(
        outbox.len() <= Outbox::DEPTH,
        "queued {} listings for a subscriber that read none",
        outbox.len()
    );
    let held = outbox.drain(channel);
    let newest = held.last().expect("something survived");
    assert_eq!(
        decode(&newest.payload),
        containers(last),
        "the survivor must be the newest listing, or coalescing would hand back stale state"
    );
    assert!(
        newest.superseded > 0,
        "and must say it is not every listing the host produced"
    );
}

#[test]
fn two_topics_on_one_session_do_not_supersede_each_other() {
    let mut session = session(&[Capability::ContainerRead, Capability::ImageRead]);
    let mut channels = Channels::new();
    let mut outbox = Outbox::new();
    let mut subscriptions = Subscriptions::new();
    follow(&mut session, Topic::Containers, &mut subscriptions, &mut channels);
    follow(&mut session, Topic::Images, &mut subscriptions, &mut channels);

    let images = Snapshot::Images(Vec::new());
    for count in 1..=200 {
        subscriptions.emit(
            Topic::Containers,
            payload(&containers(count)),
            &session,
            &mut channels,
            &mut outbox,
        );
    }
    let emission = subscriptions.emit(Topic::Images, payload(&images), &session, &mut channels, &mut outbox);

    assert_eq!(emission, Emission::Queued);
    let container_channel = subscriptions.channel(Topic::Containers).expect("routed");
    let image_channel = subscriptions.channel(Topic::Images).expect("routed");
    assert_ne!(container_channel, image_channel, "a topic gets its own channel");
    let held = outbox.drain(image_channel);
    assert_eq!(held.len(), 1);
    assert_eq!(
        decode(&held[0].payload),
        images,
        "a flood of container updates must not displace the image listing"
    );
}

#[test]
fn a_byte_stream_blocks_rather_than_dropping() {
    let mut channels = Channels::new();
    let mut outbox = Outbox::new();
    let mut streams = Streams::new();
    let channel = streams.open(&mut channels).expect("opened");

    let mut blocked = 0_u32;
    for index in 0..1_000_u32 {
        let emission = streams.write(channel, vec![index as u8], &mut channels, &mut outbox);
        if emission == Emission::Blocked {
            blocked += 1;
        }
        assert_ne!(emission, Emission::Superseded, "bytes cannot coalesce");
    }

    assert!(blocked > 0, "the producer must be stopped, not served endlessly");
    assert_eq!(outbox.dropped(), 0, "and no byte may be dropped to make room");
    assert!(outbox.len() <= Outbox::DEPTH);
}

#[test]
fn the_end_of_a_stream_is_distinguishable_from_an_empty_chunk() {
    let mut channels = Channels::new();
    let mut outbox = Outbox::new();
    let mut streams = Streams::new();
    let channel = streams.open(&mut channels).expect("opened");

    assert_eq!(
        streams.write(channel, Vec::new(), &mut channels, &mut outbox),
        Emission::Queued
    );
    assert_eq!(streams.finish(channel, &mut channels, &mut outbox), Emission::Queued);

    let held = outbox.drain(channel);
    let empty = Parcel::read(&held[0].payload).expect("a parcel");
    let end = Parcel::read(&held[1].payload).expect("a parcel");
    assert_eq!(empty, Parcel::Chunk(Vec::new()));
    assert!(!empty.ends(), "an empty chunk is 'nothing yet', not 'nothing ever'");
    assert_eq!(end, Parcel::End);
    assert!(end.ends());
}

#[test]
fn exceeding_the_stream_bound_is_refused_without_disturbing_the_open_ones() {
    let mut channels = Channels::new();
    let mut outbox = Outbox::new();
    let mut streams = Streams::new();
    let open: Vec<_> = (0..Streams::LIMIT)
        .map(|_| streams.open(&mut channels).expect("within the bound"))
        .collect();

    assert!(streams.open(&mut channels).is_err(), "the bound is a bound");

    assert_eq!(streams.len(), Streams::LIMIT);
    assert_eq!(channels.len(), Streams::LIMIT, "a refusal allocates no channel");
    for channel in &open {
        assert!(streams.contains(*channel));
        assert_eq!(
            streams.write(*channel, b"still working".to_vec(), &mut channels, &mut outbox),
            Emission::Queued,
            "a refused stream must not disturb one already open"
        );
    }
}

#[test]
fn closing_a_subscription_returns_its_channel_to_the_budget() {
    let mut session = session(&[Capability::ContainerRead]);
    let mut channels = Channels::new();
    let mut outbox = Outbox::new();
    let mut subscriptions = Subscriptions::new();

    // More times around than the channel limit, so a channel that is allocated
    // and never released would exhaust the session partway through.
    for _ in 0..(Channels::LIMIT * 4) {
        follow(&mut session, Topic::Containers, &mut subscriptions, &mut channels);
        subscriptions.emit(
            Topic::Containers,
            payload(&containers(1)),
            &session,
            &mut channels,
            &mut outbox,
        );
        subscriptions
            .close(Topic::Containers, &mut channels, &mut outbox)
            .expect("released");
    }

    assert!(channels.is_empty(), "every channel came back");
    assert!(subscriptions.is_empty());
    assert!(outbox.is_empty(), "and a closed subscription retains nothing");
}

#[test]
fn revoking_a_capability_discards_what_it_already_queued() {
    let mut channels = Channels::new();
    let mut outbox = Outbox::new();
    let mut subscriptions = Subscriptions::new();
    let mut session = session(&[Capability::ContainerRead]);

    session
        .dispatch(
            &Request::EventSubscribe {
                topic: Topic::Containers,
            },
            &services(&Host),
        )
        .expect("subscribed");
    subscriptions.open(Topic::Containers, &mut channels).expect("a channel");
    let snapshot = Snapshot::Containers(Vec::new());
    subscriptions.emit(
        Topic::Containers,
        snapshot.payload().expect("a payload"),
        &session,
        &mut channels,
        &mut outbox,
    );
    assert!(!outbox.is_empty(), "something is waiting to be sent");

    session.revoke(
        Capability::ContainerRead,
        &mut subscriptions,
        &mut channels,
        &mut outbox,
    );

    assert!(
        outbox.is_empty(),
        "a subscriber must not receive data it is no longer entitled to, even data already queued"
    );
    assert!(!session.may_emit(Topic::Containers));
    assert!(
        subscriptions.channel(Topic::Containers).is_none(),
        "the route is gone too"
    );
}
