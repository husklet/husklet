use crate::{
    Access, AccessError, AccessIdentity, Capabilities, Identity, Kind, Metadata, MetadataError, Permissions, Timestamp,
    Umask,
};

struct MetadataFixture;

impl MetadataFixture {
    fn metadata(kind: Kind, permissions: u16) -> Metadata {
        Metadata {
            identity: Identity { device: 7, inode: 11 },
            kind,
            permissions: Permissions::from_bits(permissions),
            links: 1,
            user: 1000,
            group: 2000,
            special_device: 0,
            size: 17,
            blocks_512: 8,
            block_size: 4096,
            accessed: Timestamp::new(1, 2).unwrap(),
            modified: Timestamp::new(3, 4).unwrap(),
            changed: Timestamp::new(5, 6).unwrap(),
        }
    }

    fn access_identity(user: u32, group: u32) -> AccessIdentity {
        AccessIdentity {
            user,
            group,
            supplementary_groups: Vec::new(),
            capabilities: Capabilities::default(),
        }
    }
}

#[test]
fn metadata_validates_boundaries() {
    assert_eq!(Timestamp::new(0, 1_000_000_000), Err(MetadataError::InvalidTimestamp));
    let mut metadata = MetadataFixture::metadata(Kind::Regular, 0o644);
    metadata.links = 0;
    assert_eq!(metadata.validate(), Err(MetadataError::InvalidLinkCount));
}

#[test]
fn access_selects_permissions() {
    let metadata = MetadataFixture::metadata(Kind::Regular, 0o640);
    let read = Access::from_bits(Access::READ).unwrap();
    let write = Access::from_bits(Access::WRITE).unwrap();
    let owner = MetadataFixture::access_identity(1000, 3000);
    let mut group = MetadataFixture::access_identity(3000, 3000);
    group.supplementary_groups.push(2000);
    let other = MetadataFixture::access_identity(3000, 4000);

    assert_eq!(owner.check_access(&metadata, write), Ok(()));
    assert_eq!(group.check_access(&metadata, read), Ok(()));
    assert_eq!(group.check_access(&metadata, write), Err(AccessError::PermissionDenied));
    assert_eq!(other.check_access(&metadata, read), Err(AccessError::PermissionDenied));
    assert_eq!(Access::from_bits(8), Err(AccessError::InvalidAccess));
}

#[test]
fn capability_override_rule() {
    let metadata = MetadataFixture::metadata(Kind::Regular, 0o600);
    let mut root = MetadataFixture::access_identity(0, 0);
    root.capabilities.dac_override = true;
    let execute = Access::from_bits(Access::EXECUTE).unwrap();
    let read_write = Access::from_bits(Access::READ | Access::WRITE).unwrap();

    assert_eq!(root.check_access(&metadata, read_write), Ok(()));
    assert_eq!(
        root.check_access(&metadata, execute),
        Err(AccessError::PermissionDenied)
    );
    let directory = MetadataFixture::metadata(Kind::Directory, 0o000);
    assert_eq!(root.check_access(&directory, execute), Ok(()));
}

#[test]
fn dac_read_execute() {
    let metadata = MetadataFixture::metadata(Kind::Regular, 0o000);
    let mut credentials = MetadataFixture::access_identity(3000, 3000);
    credentials.capabilities.dac_read_search = true;

    assert_eq!(
        credentials.check_access(&metadata, Access::from_bits(Access::READ).unwrap()),
        Ok(())
    );
    assert_eq!(
        credentials.check_access(&metadata, Access::from_bits(Access::WRITE).unwrap()),
        Err(AccessError::PermissionDenied)
    );
}

#[test]
fn chmod_mode_policy() {
    let metadata = MetadataFixture::metadata(Kind::Regular, 0o600);
    let owner = MetadataFixture::access_identity(1000, 2000);
    assert_eq!(
        owner
            .chmod(&metadata, Permissions::from_bits(0))
            .unwrap()
            .permissions
            .bits(),
        0
    );
    assert_eq!(
        owner
            .chmod(&metadata, Permissions::from_bits(0o2755))
            .unwrap()
            .permissions
            .bits(),
        0o2755
    );
    let outsider = MetadataFixture::access_identity(1000, 3000);
    assert_eq!(
        outsider
            .chmod(&metadata, Permissions::from_bits(0o2755))
            .unwrap()
            .permissions
            .bits(),
        0o0755
    );
}

#[test]
fn chmod_chown_setid() {
    let metadata = MetadataFixture::metadata(Kind::Regular, 0o6755);
    let outsider = MetadataFixture::access_identity(3000, 3000);
    assert_eq!(
        outsider.chmod(&metadata, Permissions::from_bits(0o644)),
        Err(AccessError::OperationNotPermitted)
    );
    let mut owner = MetadataFixture::access_identity(1000, 2000);
    owner.supplementary_groups.push(4000);
    let changed = owner.chown(&metadata, None, Some(4000)).unwrap();
    assert_eq!(changed.group, 4000);
    assert_eq!(changed.permissions.bits(), 0o0755);
    assert_eq!(
        owner.chown(&metadata, Some(3000), None),
        Err(AccessError::OperationNotPermitted)
    );
    owner.capabilities.change_owner = true;
    assert_eq!(owner.chown(&metadata, Some(3000), None).unwrap().user, 3000);
}

#[test]
fn umask_masks_value() {
    let umask = Umask::new(0o077);
    assert_eq!(umask.apply(Permissions::from_bits(0o666)).bits(), 0o600);
    assert_eq!(umask.apply(Permissions::from_bits(0o777)).bits(), 0o700);
    assert_eq!(umask.replace(0o022).bits(), 0o077);
    assert_eq!(umask.apply(Permissions::from_bits(0o666)).bits(), 0o644);
}
