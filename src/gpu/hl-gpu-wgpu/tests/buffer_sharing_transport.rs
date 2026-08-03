#![cfg(target_os = "macos")]

use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::thread;

use hl_gpu::protocol::model::descriptor::BufferDesc;
use hl_gpu::protocol::model::enums::buffer_usage;
use hl_gpu::runtime::model::sharing::{ExportId, Exports};
use hl_gpu::transport::{SubmitHeader, Verdict};
use hl_gpu::{
    BufferId, Cmd, CommandSink, ConnectionHandler, FakeClock, GlobalLedger, GpuError, GpuExecutor,
    Limits, ReadbackRequest, RemoteCommandSink, Session, TransportError,
};
use hl_gpu_wgpu::{Device, DeviceConfig, WgpuExecutor};

struct Socket(PathBuf);

impl Socket {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "hl-buffer-sharing-{}-{:?}.sock",
            std::process::id(),
            thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        Self(path)
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

struct Host {
    session: Session,
    executor: WgpuExecutor,
}

impl Host {
    fn new(device: &Device, exports: Exports, global: GlobalLedger) -> Self {
        let executor = device.executor();
        let session = Session::new(
            Limits::from_capabilities(executor.capabilities()),
            global,
            Box::new(FakeClock::new(0)),
        )
        .with_exports(exports);
        Self { session, executor }
    }
}

impl ConnectionHandler for Host {
    fn submit(&mut self, _: &SubmitHeader, batch: &[Cmd]) -> Verdict {
        match hl_gpu::runtime::submit(&mut self.session, &mut self.executor, 0, batch) {
            Ok(_) => Verdict::Ack,
            Err(error) => Verdict::for_error(&error),
        }
    }

    fn read_buffer(&mut self, request: &ReadbackRequest) -> Option<Vec<u8>> {
        hl_gpu::runtime::service::dispatch::read_buffer(
            &self.session,
            &self.executor,
            BufferId(request.id),
            request.offset,
            request.len as usize,
        )
        .ok()
    }

    fn export_buffer(&mut self, request: &ReadbackRequest) -> Option<ExportId> {
        hl_gpu::runtime::service::dispatch::export_buffer(
            &mut self.session,
            &self.executor,
            BufferId(request.id),
        )
        .ok()
    }

    fn import_buffer(&mut self, request: &ReadbackRequest) -> Option<u64> {
        hl_gpu::runtime::service::dispatch::import_buffer(
            &mut self.session,
            &self.executor,
            BufferId(request.id),
            ExportId(request.offset),
        )
        .ok()
    }
}

#[test]
fn two_real_connections_alias_one_buffer_and_refuse_bad_identities() {
    let device = Device::new(DeviceConfig::default()).expect("Metal adapter");
    let caps = device.executor().capabilities().clone();
    let exports = Exports::new();
    let global = GlobalLedger::unbounded();
    let socket = Socket::new();
    let listener = UnixListener::bind(&socket.0).unwrap();
    let server = thread::spawn(move || {
        let mut workers = Vec::new();
        for stream in listener.incoming().take(2) {
            let stream = stream.unwrap();
            let device = device.clone();
            let exports = exports.clone();
            let global = global.clone();
            let caps = caps.clone();
            workers.push(thread::spawn(move || {
                let mut host = Host::new(&device, exports, global);
                hl_gpu::serve_connection_with_handler(&stream, &caps, &mut host).unwrap();
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
    });

    let path = socket.0.to_string_lossy().into_owned();
    let mut owner = RemoteCommandSink::new(&path);
    owner
        .submit(&[
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 13,
                    usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: b"owner-visible".to_vec(),
            },
        ])
        .unwrap();
    let export = owner.export_buffer(BufferId(1)).expect("live export");
    assert_eq!(owner.export_buffer(BufferId(1)).unwrap(), export);

    let mut importer = RemoteCommandSink::new(&path);
    assert!(matches!(
        importer.import_buffer(BufferId(2), ExportId(u64::MAX)),
        Err(GpuError::Transport(TransportError::Rejected { .. }))
    ));
    importer
        .submit(&[Cmd::CreateBuffer(
            9,
            BufferDesc {
                size: 4,
                usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
                label: String::new(),
            },
        )])
        .unwrap();
    assert!(matches!(
        importer.import_buffer(BufferId(9), export),
        Err(GpuError::Transport(TransportError::Rejected { .. }))
    ));

    assert_eq!(importer.import_buffer(BufferId(2), export).unwrap(), 13);
    assert_eq!(
        importer.read_buffer(BufferId(2), 0, 13).unwrap(),
        b"owner-visible"
    );
    assert!(matches!(
        importer.import_buffer(BufferId(3), export),
        Err(GpuError::Transport(TransportError::Rejected { .. }))
    ));

    importer
        .submit(&[Cmd::WriteBuffer {
            id: 2,
            offset: 0,
            data: b"alias-visible".to_vec(),
        }])
        .unwrap();
    assert_eq!(
        owner.read_buffer(BufferId(1), 0, 13).unwrap(),
        b"alias-visible"
    );

    drop(importer);
    drop(owner);
    server.join().unwrap();
}
