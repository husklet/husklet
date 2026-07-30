//! Virtual Linux render node required by DRM-aware Wayland clients.

use hl_container::device::extension::{
    DeviceEntry, DeviceKind, ExtensionConfig, ExtensionSpec, Feature, FileEntry, FileSource,
    HandleOperation, Handles, Interest, IoctlRequest, IoctlResult, IoctlWrite, LinuxError,
    Metadata, NamespaceEntry, OpenHandle, OpenRequest, ProviderId, ReadRequest, Readiness,
    ServiceId, ServiceRegistration, SymlinkEntry, WriteRequest,
};
use hl_container::{Authority, DeviceRequest};
use std::collections::BTreeSet;
use std::sync::Arc;

const SERVICE: ServiceId = ServiceId(1);
const DRM_VERSION: u64 = 0xc040_6400;
const DRM_GET_CAP: u64 = 0xc010_640c;
const DRM_SET_CLIENT_CAP: u64 = 0x4010_640d;
const DRM_CAP_PRIME: u64 = 0x5;
const DRM_CAP_TIMESTAMP_MONOTONIC: u64 = 0x6;
const DRM_CAP_SYNCOBJ: u64 = 0x13;
const DRM_CAP_SYNCOBJ_TIMELINE: u64 = 0x14;
const DRM_PRIME_HANDLE_TO_FD: u64 = 0xc00c_642d;
const DRM_PRIME_FD_TO_HANDLE: u64 = 0xc00c_642e;
const DRM_SYNCOBJ_CREATE: u64 = 0xc008_64bf;
const DRM_SYNCOBJ_DESTROY: u64 = 0xc008_64c0;
const DRM_SYNCOBJ_HANDLE_TO_FD: u64 = 0xc018_64c1;
const DRM_SYNCOBJ_FD_TO_HANDLE: u64 = 0xc018_64c2;
const DRM_SYNCOBJ_WAIT: u64 = 0xc028_64c3;
const DRM_SYNCOBJ_RESET: u64 = 0xc010_64c4;
const DRM_SYNCOBJ_SIGNAL: u64 = 0xc010_64c5;
const DRM_SYNCOBJ_TIMELINE_WAIT: u64 = 0xc030_64ca;
const DRM_SYNCOBJ_QUERY: u64 = 0xc018_64cb;
const DRM_SYNCOBJ_TRANSFER: u64 = 0xc020_64cc;
const DRM_SYNCOBJ_TIMELINE_SIGNAL: u64 = 0xc018_64cd;
const DRM_SYNCOBJ_EVENTFD: u64 = 0xc018_64cf;

pub(super) struct RenderNode;

impl RenderNode {
    pub(super) fn install(request: &mut DeviceRequest) -> std::io::Result<()> {
        let namespace = request
            .extensions
            .iter_mut()
            .find(|extension| extension.provider.as_str() == "engine.namespace")
            .ok_or_else(|| std::io::Error::other("graphics namespace extension is missing"))?;
        namespace.namespace.extend(Self::topology());
        namespace.required_features.extend([
            feature("directories")?,
            feature("immutable-files")?,
            feature("symlinks")?,
        ]);

        let provider = provider("engine.handles")?;
        request.extensions.push(ExtensionSpec {
            provider: provider.clone(),
            version: hl_container::device::Version::new(1, 0),
            required: true,
            required_features: BTreeSet::from([
                feature("devices")?,
                feature("ioctl")?,
                feature("metadata")?,
                feature("poll")?,
                feature("read")?,
                feature("write")?,
                feature("ofd-lifecycle")?,
            ]),
            optional_features: BTreeSet::new(),
            config: ExtensionConfig::empty("engine.handles/v1"),
            namespace: vec![NamespaceEntry::Device(DeviceEntry {
                path: "/dev/dri/renderD128".into(),
                metadata: metadata(0o666),
                kind: DeviceKind::Character,
                major: 226,
                minor: 128,
                service: Some(SERVICE),
            })],
            rules: Vec::new(),
            services: vec![ServiceRegistration {
                id: SERVICE,
                operations: BTreeSet::from([
                    HandleOperation::Read,
                    HandleOperation::Write,
                    HandleOperation::Metadata,
                    HandleOperation::Ioctl,
                    HandleOperation::Poll,
                ]),
                max_request_bytes: 4096,
            }],
            memory: Vec::new(),
            environment: Vec::new(),
        });
        request
            .authorities
            .push(Authority::new(provider).handles(Arc::new(Self)));
        Ok(())
    }

    fn topology() -> Vec<NamespaceEntry> {
        [
            "/dev/dri",
            "/sys",
            "/sys/class",
            "/sys/class/drm",
            "/sys/dev",
            "/sys/dev/char",
            "/sys/devices",
            "/sys/devices/platform",
            "/sys/devices/platform/hl-gpu",
            "/sys/devices/platform/hl-gpu/drm",
            "/sys/devices/platform/hl-gpu/drm/renderD128",
            "/sys/bus",
            "/sys/bus/platform",
        ]
        .into_iter()
        .map(|path| {
            NamespaceEntry::Directory(hl_container::device::extension::DirectoryEntry {
                path: path.into(),
                metadata: metadata(0o755),
            })
        })
        .chain([
            file(
                "/sys/devices/platform/hl-gpu/drm/renderD128/dev",
                b"226:128\n",
            ),
            file(
                "/sys/devices/platform/hl-gpu/drm/renderD128/uevent",
                b"MAJOR=226\nMINOR=128\nDEVNAME=dri/renderD128\n",
            ),
            file(
                "/sys/devices/platform/hl-gpu/uevent",
                b"DRIVER=hl_gpu\n\
                  OF_NAME=gpu\n\
                  OF_FULLNAME=/hl-gpu\n\
                  OF_COMPATIBLE_N=1\n\
                  OF_COMPATIBLE_0=husklet,gpu\n\
                  MODALIAS=platform:hl-gpu\n",
            ),
            symlink(
                "/sys/devices/platform/hl-gpu/subsystem",
                "/sys/bus/platform",
            ),
            symlink(
                "/sys/devices/platform/hl-gpu/drm/renderD128/device",
                "../..",
            ),
            symlink(
                "/sys/devices/platform/hl-gpu/drm/renderD128/subsystem",
                "/sys/class/drm",
            ),
            symlink(
                "/sys/class/drm/renderD128",
                "/sys/devices/platform/hl-gpu/drm/renderD128",
            ),
            symlink(
                "/sys/dev/char/226:128",
                "/sys/devices/platform/hl-gpu/drm/renderD128",
            ),
        ])
        .collect()
    }
}

impl Handles for RenderNode {
    fn open(&self, request: OpenRequest) -> Result<Box<dyn OpenHandle>, LinuxError> {
        hl_log::hl_log!(
            hl_log::tag::GPU,
            hl_log::Level::Debug,
            "render-node open service={:?} access={:?}",
            request.service,
            request.access
        );
        if request.service != SERVICE {
            return Err(linux(libc::ENODEV, "unknown graphics service"));
        }
        Ok(Box::new(RenderHandle))
    }
}

struct RenderHandle;

impl OpenHandle for RenderHandle {
    fn read(&self, _request: ReadRequest) -> Result<Vec<u8>, LinuxError> {
        Err(linux(libc::EINVAL, "render node is not byte-readable"))
    }

    fn write(&self, _request: WriteRequest) -> Result<usize, LinuxError> {
        Err(linux(libc::EINVAL, "render node is not byte-writable"))
    }

    fn metadata(&self) -> Result<hl_container::device::extension::HandleMetadata, LinuxError> {
        Ok(hl_container::device::extension::HandleMetadata {
            metadata: metadata(0o666),
            size: 0,
        })
    }

    fn ioctl(&self, request: IoctlRequest) -> Result<IoctlResult, LinuxError> {
        let command = request.command;
        hl_log::hl_log!(
            hl_log::tag::GPU,
            hl_log::Level::Debug,
            "render-node ioctl begin command=0x{:x} bytes={}",
            command,
            request.argument.len()
        );
        let result = match command {
            DRM_VERSION => version(request.argument),
            DRM_GET_CAP => capability(request.argument),
            DRM_SET_CLIENT_CAP => client_capability(request.argument),
            DRM_PRIME_HANDLE_TO_FD
            | DRM_PRIME_FD_TO_HANDLE
            | DRM_SYNCOBJ_CREATE
            | DRM_SYNCOBJ_DESTROY
            | DRM_SYNCOBJ_HANDLE_TO_FD
            | DRM_SYNCOBJ_FD_TO_HANDLE
            | DRM_SYNCOBJ_WAIT
            | DRM_SYNCOBJ_RESET
            | DRM_SYNCOBJ_SIGNAL
            | DRM_SYNCOBJ_TIMELINE_WAIT
            | DRM_SYNCOBJ_QUERY
            | DRM_SYNCOBJ_TRANSFER
            | DRM_SYNCOBJ_TIMELINE_SIGNAL
            | DRM_SYNCOBJ_EVENTFD => Err(linux(
                libc::EOPNOTSUPP,
                "render-node sharing and synchronization are not implemented",
            )),
            _ => Err(linux(libc::ENOTTY, "unsupported render-node ioctl")),
        };
        hl_log::hl_log!(
            hl_log::tag::GPU,
            hl_log::Level::Debug,
            "render-node ioctl end command=0x{:x} result={:?}",
            command,
            result.as_ref().map(|value| value.value)
        );
        result
    }

    fn readiness(&self, _interest: Interest) -> Result<Readiness, LinuxError> {
        // Linux drm_poll reports readable only while the per-open event list is non-empty. This
        // render-only projection produces no DRM events, so claiming perpetual readiness would make
        // epoll clients spin even though read cannot consume anything.
        Ok(Readiness {
            states: BTreeSet::new(),
        })
    }

    fn flush(&self) -> Result<(), LinuxError> {
        Ok(())
    }

    fn close(self: Box<Self>) {}
}

fn version(mut argument: Vec<u8>) -> Result<IoctlResult, LinuxError> {
    if argument.len() != 64 {
        return Err(linux(libc::EINVAL, "DRM version argument must be 64 bytes"));
    }
    argument[0..4].copy_from_slice(&1_i32.to_ne_bytes());
    argument[4..8].copy_from_slice(&0_i32.to_ne_bytes());
    argument[8..12].copy_from_slice(&0_i32.to_ne_bytes());
    let values = [
        (16, 24, b"hl_gpu".as_slice()),
        (32, 40, b"0".as_slice()),
        (48, 56, b"Husklet projected render node".as_slice()),
    ];
    let mut writes = Vec::new();
    for (length, pointer, value) in values {
        let capacity = u64::from_ne_bytes(argument[length..length + 8].try_into().unwrap());
        let address = u64::from_ne_bytes(argument[pointer..pointer + 8].try_into().unwrap());
        hl_log::hl_log!(
            hl_log::tag::GPU,
            hl_log::Level::Debug,
            "DRM_VERSION field length_offset={} capacity={} address=0x{:x} value_bytes={}",
            length,
            capacity,
            address,
            value.len()
        );
        argument[length..length + 8].copy_from_slice(&(value.len() as u64).to_ne_bytes());
        if address != 0 && capacity != 0 {
            writes.push(IoctlWrite {
                address,
                bytes: value[..value.len().min(capacity as usize)].to_vec(),
            });
        }
    }
    hl_log::hl_log!(
        hl_log::tag::GPU,
        hl_log::Level::Debug,
        "DRM_VERSION reply version=1.0.0 writes={:?}",
        writes
            .iter()
            .map(|write| (write.address, write.bytes.len()))
            .collect::<Vec<_>>()
    );
    Ok(IoctlResult {
        value: 0,
        argument,
        writes,
    })
}

fn capability(mut argument: Vec<u8>) -> Result<IoctlResult, LinuxError> {
    if argument.len() != 16 {
        return Err(linux(
            libc::EINVAL,
            "DRM capability argument must be 16 bytes",
        ));
    }
    let cap = u64::from_ne_bytes(argument[..8].try_into().unwrap());
    let value: u64 = match cap {
        // Linux DRM core answers this for render-only devices. It describes the clock used if an event
        // carries a timestamp; it does not claim that this node implements vblank or page-flip events.
        DRM_CAP_TIMESTAMP_MONOTONIC => 1,
        // Husklet's GBM shim owns its buffers directly. This node implements neither PRIME conversion
        // ioctl, so importing/exporting GEM handles must not be advertised.
        DRM_CAP_PRIME | DRM_CAP_SYNCOBJ | DRM_CAP_SYNCOBJ_TIMELINE => 0,
        // Every remaining core capability is either KMS-only or unknown. A Linux render-only DRM device
        // rejects these before entering the KMS capability switch.
        _ => {
            return Err(linux(
                libc::EOPNOTSUPP,
                "capability is unavailable on a render-only DRM device",
            ));
        }
    };
    argument[8..16].copy_from_slice(&value.to_ne_bytes());
    Ok(IoctlResult {
        value: 0,
        argument,
        writes: Vec::new(),
    })
}

fn client_capability(argument: Vec<u8>) -> Result<IoctlResult, LinuxError> {
    if argument.len() != 16 {
        return Err(linux(
            libc::EINVAL,
            "DRM client capability argument must be 16 bytes",
        ));
    }
    Err(linux(
        libc::EACCES,
        "DRM_SET_CLIENT_CAP is not permitted on a render node",
    ))
}

fn file(path: &str, bytes: &'static [u8]) -> NamespaceEntry {
    NamespaceEntry::File(FileEntry {
        path: path.into(),
        metadata: metadata(0o444),
        source: FileSource::Immutable(Arc::from(bytes)),
    })
}

fn symlink(path: &str, target: &str) -> NamespaceEntry {
    NamespaceEntry::Symlink(SymlinkEntry {
        path: path.into(),
        target: target.into(),
        uid: 0,
        gid: 0,
    })
}

const fn metadata(mode: u32) -> Metadata {
    Metadata {
        mode,
        uid: 0,
        gid: 0,
    }
}

fn provider(value: &str) -> std::io::Result<ProviderId> {
    ProviderId::new(value).map_err(|error| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{error:?}"))
    })
}

fn feature(value: &str) -> std::io::Result<Feature> {
    Feature::new(value).map_err(|error| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{error:?}"))
    })
}

fn linux(errno: i32, context: &str) -> LinuxError {
    LinuxError {
        errno,
        context: context.to_owned(),
    }
}

#[cfg(test)]
#[path = "render_test.rs"]
mod tests;
