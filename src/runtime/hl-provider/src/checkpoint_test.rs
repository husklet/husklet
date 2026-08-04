use crate::{
    HandleKind, HandleNamespace, NamespaceError, ProviderCheckpointCapture, ProviderCheckpointImage,
    ProviderClientCheckpoint, ProviderResourceKey, RemoteId,
};

struct Capture;

impl ProviderCheckpointCapture for Capture {
    fn freeze(&self) -> Result<(), NamespaceError> {
        Ok(())
    }
    fn thaw(&self) {}

    fn resource_key(&self, slot: usize, _: RemoteId) -> Result<ProviderResourceKey, NamespaceError> {
        ProviderResourceKey::new(slot as u64 + 1).ok_or(NamespaceError::InvalidSnapshot)
    }

    fn projected_state(
        &self,
    ) -> Result<(Vec<crate::ProviderFileCheckpoint>, ProviderClientCheckpoint), NamespaceError> {
        Ok((
            Vec::new(),
            ProviderClientCheckpoint {
                request_generations: vec![0, 7],
                subscription_generations: vec![0, 4],
                next_request: 9,
                next_subscription: 5,
                late_replies: 2,
                stale_events: 3,
                subscriptions: Vec::new(),
            },
        ))
    }
}

#[test]
fn aggregate_round_trip() {
    let namespace = HandleNamespace::new(3).unwrap();
    let stale = namespace.open(RemoteId::new(10).unwrap(), HandleKind::File).unwrap();
    assert_eq!(
        namespace.close(stale).unwrap().unwrap().remote(),
        RemoteId::new(10).unwrap()
    );
    let handle = namespace.open(RemoteId::new(11).unwrap(), HandleKind::File).unwrap();
    namespace.clone_handle(handle).unwrap();
    namespace.freeze_checkpoint();
    let image = ProviderCheckpointImage::capture(&namespace, &Capture).unwrap();
    namespace.thaw_checkpoint();
    assert_eq!(image.namespace.entries[0].references, 2);
    assert_eq!(image.namespace.generations[0], 2);

    let restored = HandleNamespace::restore_checkpoint(&image.namespace, &[(0, RemoteId::new(101).unwrap())]).unwrap();
    assert_eq!(restored.snapshot().entries[0].remote, RemoteId::new(101).unwrap());
    let closes = restored.revoke();
    assert_eq!(closes.len(), 1);
    assert_eq!(closes[0].remote(), RemoteId::new(101).unwrap());
}

#[test]
fn validation_rejects_duplicate() {
    let namespace = HandleNamespace::new(2).unwrap();
    namespace.open(RemoteId::new(1).unwrap(), HandleKind::File).unwrap();
    namespace.open(RemoteId::new(2).unwrap(), HandleKind::Event).unwrap();
    namespace.freeze_checkpoint();
    let image = ProviderCheckpointImage::capture(&namespace, &Capture).unwrap();
    namespace.thaw_checkpoint();

    let mut duplicate = image.clone();
    duplicate.resources[1].key = duplicate.resources[0].key;
    assert_eq!(duplicate.validate(), Err(NamespaceError::InvalidSnapshot));

    let mut stale = image.clone();
    stale.namespace.entries[0].generation = stale.namespace.entries[0].generation.wrapping_add(1);
    assert_eq!(stale.validate(), Err(NamespaceError::InvalidSnapshot));

    let mut malformed = image;
    malformed.client.next_request = 0;
    assert_eq!(malformed.validate(), Err(NamespaceError::InvalidSnapshot));
}
