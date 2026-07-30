use super::*;

const DRM_CAP_DUMB_BUFFER: u64 = 0x1;
const DRM_CAP_VBLANK_HIGH_CRTC: u64 = 0x2;
const DRM_CAP_DUMB_PREFERRED_DEPTH: u64 = 0x3;
const DRM_CAP_ADDFB2_MODIFIERS: u64 = 0x10;

fn get_cap(capability: u64) -> Vec<u8> {
    let mut argument = vec![0; 16];
    argument[..8].copy_from_slice(&capability.to_ne_bytes());
    argument
}

fn value(result: &IoctlResult) -> u64 {
    u64::from_ne_bytes(result.argument[8..16].try_into().unwrap())
}

#[test]
fn sharing_and_sync_commands_match_the_linux_drm_uapi() {
    assert_eq!(DRM_PRIME_HANDLE_TO_FD, 0xc00c_642d);
    assert_eq!(DRM_PRIME_FD_TO_HANDLE, 0xc00c_642e);
    assert_eq!(DRM_SYNCOBJ_CREATE, 0xc008_64bf);
    assert_eq!(DRM_SYNCOBJ_DESTROY, 0xc008_64c0);
    assert_eq!(DRM_SYNCOBJ_HANDLE_TO_FD, 0xc018_64c1);
    assert_eq!(DRM_SYNCOBJ_FD_TO_HANDLE, 0xc018_64c2);
    assert_eq!(DRM_SYNCOBJ_WAIT, 0xc028_64c3);
    assert_eq!(DRM_SYNCOBJ_RESET, 0xc010_64c4);
    assert_eq!(DRM_SYNCOBJ_SIGNAL, 0xc010_64c5);
    assert_eq!(DRM_SYNCOBJ_TIMELINE_WAIT, 0xc030_64ca);
    assert_eq!(DRM_SYNCOBJ_QUERY, 0xc018_64cb);
    assert_eq!(DRM_SYNCOBJ_TRANSFER, 0xc020_64cc);
    assert_eq!(DRM_SYNCOBJ_TIMELINE_SIGNAL, 0xc018_64cd);
    assert_eq!(DRM_SYNCOBJ_EVENTFD, 0xc018_64cf);
}

#[test]
fn version_matches_the_64_bit_drm_uapi_layout() {
    let mut argument = vec![0; 64];
    let fields = [
        (16, 24, 0x1000_u64, 32_u64),
        (32, 40, 0x2000_u64, 32_u64),
        (48, 56, 0x3000_u64, 64_u64),
    ];
    for (length, pointer, address, capacity) in fields {
        argument[length..length + 8].copy_from_slice(&capacity.to_ne_bytes());
        argument[pointer..pointer + 8].copy_from_slice(&address.to_ne_bytes());
    }

    let result = version(argument).unwrap();

    assert_eq!(
        i32::from_ne_bytes(result.argument[0..4].try_into().unwrap()),
        1
    );
    assert_eq!(
        i32::from_ne_bytes(result.argument[4..8].try_into().unwrap()),
        0
    );
    assert_eq!(
        i32::from_ne_bytes(result.argument[8..12].try_into().unwrap()),
        0
    );
    let expected = [
        (16, 0x1000, b"hl_gpu".as_slice()),
        (32, 0x2000, b"0".as_slice()),
        (48, 0x3000, b"Husklet projected render node".as_slice()),
    ];
    for ((length, address, bytes), write) in expected.into_iter().zip(&result.writes) {
        assert_eq!(
            u64::from_ne_bytes(result.argument[length..length + 8].try_into().unwrap()),
            bytes.len() as u64
        );
        assert_eq!(write.address, address);
        assert_eq!(write.bytes, bytes);
    }
}

#[test]
fn version_respects_null_pointers_and_caller_capacities() {
    let mut argument = vec![0; 64];
    argument[16..24].copy_from_slice(&3_u64.to_ne_bytes());
    argument[24..32].copy_from_slice(&0x1000_u64.to_ne_bytes());
    argument[32..40].copy_from_slice(&32_u64.to_ne_bytes());

    let result = version(argument).unwrap();

    assert_eq!(result.writes.len(), 1);
    assert_eq!(result.writes[0].address, 0x1000);
    assert_eq!(result.writes[0].bytes, b"hl_");
    assert_eq!(
        u64::from_ne_bytes(result.argument[16..24].try_into().unwrap()),
        b"hl_gpu".len() as u64
    );
    assert_eq!(
        u64::from_ne_bytes(result.argument[32..40].try_into().unwrap()),
        1
    );
}

#[test]
fn version_rejects_non_uapi_argument_lengths() {
    for length in [0, 63, 65] {
        let error = version(vec![0; length]).unwrap_err();
        assert_eq!(error.errno, libc::EINVAL);
    }
}

#[test]
fn render_only_capabilities_are_truthful() {
    let monotonic = capability(get_cap(DRM_CAP_TIMESTAMP_MONOTONIC)).unwrap();
    assert_eq!(value(&monotonic), 1);

    for cap in [DRM_CAP_PRIME, DRM_CAP_SYNCOBJ, DRM_CAP_SYNCOBJ_TIMELINE] {
        let unsupported = capability(get_cap(cap)).unwrap();
        assert_eq!(value(&unsupported), 0, "capability {cap:#x}");
    }
}

#[test]
fn kms_and_unknown_capabilities_are_not_fabricated() {
    for cap in [
        DRM_CAP_DUMB_BUFFER,
        DRM_CAP_VBLANK_HIGH_CRTC,
        DRM_CAP_DUMB_PREFERRED_DEPTH,
        DRM_CAP_ADDFB2_MODIFIERS,
        u64::MAX,
    ] {
        let error = capability(get_cap(cap)).unwrap_err();
        assert_eq!(error.errno, libc::EOPNOTSUPP, "capability {cap:#x}");
    }
}

#[test]
fn capability_rejects_non_uapi_argument_lengths() {
    for length in [0, 15, 17] {
        let error = capability(vec![0; length]).unwrap_err();
        assert_eq!(error.errno, libc::EINVAL);
    }
}

#[test]
fn render_node_rejects_all_set_client_capabilities() {
    let error = client_capability(vec![0; 16]).unwrap_err();
    assert_eq!(error.errno, libc::EACCES);

    for length in [0, 15, 17] {
        let error = client_capability(vec![0; length]).unwrap_err();
        assert_eq!(error.errno, libc::EINVAL);
    }
}

#[test]
fn handle_metadata_matches_the_projected_device_permissions() {
    let actual = OpenHandle::metadata(&RenderHandle).unwrap();

    assert_eq!(actual.metadata, metadata(0o666));
    assert_eq!(actual.size, 0);
}

#[test]
fn sharing_and_sync_ioctls_are_recognized_but_not_fabricated() {
    let handle = RenderHandle;
    for command in [
        DRM_PRIME_HANDLE_TO_FD,
        DRM_PRIME_FD_TO_HANDLE,
        DRM_SYNCOBJ_CREATE,
        DRM_SYNCOBJ_DESTROY,
        DRM_SYNCOBJ_HANDLE_TO_FD,
        DRM_SYNCOBJ_FD_TO_HANDLE,
        DRM_SYNCOBJ_WAIT,
        DRM_SYNCOBJ_RESET,
        DRM_SYNCOBJ_SIGNAL,
        DRM_SYNCOBJ_TIMELINE_WAIT,
        DRM_SYNCOBJ_QUERY,
        DRM_SYNCOBJ_TRANSFER,
        DRM_SYNCOBJ_TIMELINE_SIGNAL,
        DRM_SYNCOBJ_EVENTFD,
    ] {
        let error = handle
            .ioctl(IoctlRequest {
                command,
                argument: Vec::new(),
                deadline: std::time::SystemTime::now(),
            })
            .unwrap_err();
        assert_eq!(error.errno, libc::EOPNOTSUPP, "command {command:#x}");
    }

    let error = handle
        .ioctl(IoctlRequest {
            command: 0xffff_ffff,
            argument: Vec::new(),
            deadline: std::time::SystemTime::now(),
        })
        .unwrap_err();
    assert_eq!(error.errno, libc::ENOTTY);
}

#[test]
fn render_node_does_not_claim_events_that_it_cannot_deliver() {
    let readiness = RenderHandle
        .readiness(Interest {
            readable: true,
            writable: true,
            priority: true,
        })
        .unwrap();

    assert!(readiness.states.is_empty());
}
