use super::*;
use crate::model::context::{GlContext, GlSurface};
use crate::service::{record, swap};
use hl_gpu::protocol::model::capability::{Capabilities, FeatureRequest};
use hl_gpu::protocol::model::command::Cmd;
use hl_gpu::protocol::model::id::{BufferId, FenceId};
use hl_gpu::{CommandSink, Result};

struct Sink {
    log: Rc<RefCell<Vec<Rec>>>,
}

impl CommandSink for Sink {
    fn negotiate(&mut self, _request: &FeatureRequest) -> Result<Capabilities> {
        Ok(Capabilities::permissive_fixture("native-presentation-test"))
    }

    fn submit(&mut self, batch: &[Cmd]) -> Result<()> {
        self.log.borrow_mut().push(Rec::GpuSubmit {
            native_present: batch
                .iter()
                .any(|command| matches!(command, Cmd::Present { .. })),
        });
        Ok(())
    }

    fn wait(&mut self, _fence: FenceId, _value: u64) -> Result<()> {
        Ok(())
    }

    fn read_buffer(&mut self, _id: BufferId, _offset: u64, len: usize) -> Result<Vec<u8>> {
        self.log.borrow_mut().push(Rec::GpuRead);
        Ok(vec![0; len])
    }
}

fn context(frame: NativeFrame) -> GlContext {
    let mut context = GlContext::new();
    context.local.surf = GlSurface {
        have: true,
        width: 4,
        height: 3,
    };
    context.local.present_token = Some(frame.token);
    context.local.present_serial = Some(frame.serial);
    context
}

#[test]
fn no_op_native_swap_never_associates_or_commits() {
    let recorder = Box::new(Recorder::new());
    let log = Rc::clone(&recorder.log);
    let mut presenter = WaylandAppPresenter::with_abi(recorder, SURFACE).expect("bring-up");
    let frame = presenter.reserve_native_frame().expect("identity");
    log.borrow_mut().clear();
    let mut sink = Sink {
        log: Rc::clone(&log),
    };

    let submitted = swap::swap_buffers(&mut context(frame), &mut sink).expect("swap");
    if submitted {
        presenter
            .commit_native(frame, 4, 3)
            .expect("commit after GPU submission");
    }

    assert!(!submitted);
    assert!(log.borrow().iter().all(|event| !matches!(
        event,
        Rec::GpuSubmit { .. }
            | Rec::GpuRead
            | Rec::Associate(_)
            | Rec::Commit { .. }
            | Rec::ShmCreatePool { .. }
    )));
}

#[test]
fn native_swap_submits_before_associate_and_commit_without_readback_or_shm() {
    let recorder = Box::new(Recorder::new());
    let log = Rc::clone(&recorder.log);
    let mut presenter = WaylandAppPresenter::with_abi(recorder, SURFACE).expect("bring-up");
    let frame = presenter.reserve_native_frame().expect("identity");
    log.borrow_mut().clear();
    let mut sink = Sink {
        log: Rc::clone(&log),
    };
    let mut context = context(frame);
    record::clear(&mut context);

    let submitted = swap::swap_buffers(&mut context, &mut sink).expect("swap");
    if submitted {
        presenter
            .commit_native(frame, 4, 3)
            .expect("commit after GPU submission");
    }

    let log = log.borrow();
    let submit = log
        .iter()
        .position(|event| {
            matches!(
                event,
                Rec::GpuSubmit {
                    native_present: true
                }
            )
        })
        .expect("native GPU submission");
    let associate = log
        .iter()
        .position(|event| *event == Rec::Associate(1))
        .expect("surface association");
    let commit = log
        .iter()
        .position(|event| matches!(event, Rec::Commit { .. }))
        .expect("surface commit");
    assert!(submit < associate && associate < commit);
    assert!(log.iter().all(|event| !matches!(
        event,
        Rec::GpuRead
            | Rec::ShmCreatePool { .. }
            | Rec::PoolCreateBuffer { .. }
            | Rec::Attach { .. }
    )));
}

#[test]
fn presenters_keep_tokens_and_serials_isolated_per_surface() {
    let mut first_recorder = Recorder::new();
    first_recorder.identity_token = 11;
    let mut second_recorder = Recorder::new();
    second_recorder.identity_token = 22;
    let mut first =
        WaylandAppPresenter::with_abi(Box::new(first_recorder), SURFACE).expect("first");
    let mut second =
        WaylandAppPresenter::with_abi(Box::new(second_recorder), 0xA9910usize as *mut c_void)
            .expect("second");

    let first_frame = first.reserve_native_frame().expect("first identity");
    let second_frame = second.reserve_native_frame().expect("second identity");
    assert_eq!(first_frame.token.get(), 11);
    assert_eq!(second_frame.token.get(), 22);
    assert_eq!(first_frame.serial.get(), 1);
    assert_eq!(second_frame.serial.get(), 1);
    assert_eq!(first.reserve_native_frame().unwrap().serial.get(), 2);
    assert_eq!(second.reserve_native_frame().unwrap().serial.get(), 2);
}

#[test]
fn native_frame_associates_only_when_explicitly_committed() {
    let rec = Box::new(Recorder::new());
    let mut presenter = WaylandAppPresenter::with_abi(rec, SURFACE).expect("bring-up");
    let frame = presenter.reserve_native_frame().expect("identity");
    assert_eq!(frame.serial.get(), 1);
    let log = unsafe { &*(std::ptr::addr_of!(*presenter.abi) as *const Recorder) };
    assert!(
        !log.log()
            .iter()
            .any(|event| matches!(event, Rec::Associate(_))),
        "reservation before GPU submit must not marshal associate"
    );

    presenter
        .commit_native(frame, 4, 3)
        .expect("commit after successful submit");
    let log = log.log();
    let associate = log
        .iter()
        .position(|event| *event == Rec::Associate(1))
        .expect("associate");
    let commit = log
        .iter()
        .position(|event| matches!(event, Rec::Commit { .. }))
        .expect("commit");
    assert!(associate < commit);
    assert_eq!(presenter.reserve_native_frame().unwrap().serial.get(), 2);
}

#[test]
fn missing_identity_preserves_app_surface_shm() {
    let mut recorder = Recorder::new();
    recorder.has_identity = false;
    let mut presenter = WaylandAppPresenter::with_abi(Box::new(recorder), SURFACE).expect("SHM");
    assert!(presenter.reserve_native_frame().is_none());
    presenter.present(&xrgb(2, 2), 2, 2).expect("SHM fallback");
}

#[test]
fn drop_retires_identity_before_wrapper_and_owned_queue() {
    let recorder = Box::new(Recorder::new());
    let log = Rc::clone(&recorder.log);
    let presenter = WaylandAppPresenter::with_abi(recorder, SURFACE).expect("bring-up");
    drop(presenter);
    let log = log.borrow();
    let identity = log
        .iter()
        .position(|event| *event == Rec::DestroyIdentity)
        .expect("identity destroy");
    let wrapper = log
        .iter()
        .rposition(|event| matches!(event, Rec::WrapperDestroy(_)))
        .expect("surface wrapper destroy");
    let queue = log
        .iter()
        .position(|event| matches!(event, Rec::DestroyQueue(_)))
        .expect("queue destroy");
    assert!(identity < wrapper && wrapper < queue);
}
