use super::*;
use crate::state::plan::SubmitPlan;
use hl_gpu::{
    BufferId, Capabilities, Cmd, CommandSink, FeatureRequest, FenceId, FenceWait, GpuError,
};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

fn group(allocator: &Arc<IrAllocator>) -> Arc<GroupSlot> {
    GroupSlot::new(Arc::clone(allocator))
}

#[test]
fn unrelated_groups_do_not_convoy() {
    let allocator = Arc::new(IrAllocator::new());
    let blocked = group(&allocator);
    let independent = group(&allocator);
    let lease = blocked.acquire().expect("blocked lease");
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let _lease = independent.acquire().expect("independent lease");
        done_tx.send(()).expect("done");
    });

    done_rx
        .recv_timeout(Duration::from_millis(100))
        .expect("unrelated group remained usable");
    drop(lease);
}

#[test]
fn same_group_waits_and_wakes_after_release() {
    let slot = group(&Arc::new(IrAllocator::new()));
    let lease = slot.acquire().expect("first");
    let waiting = Arc::clone(&slot);
    let (done_tx, done_rx) = mpsc::channel();
    let thread = thread::spawn(move || {
        drop(waiting.acquire().expect("second"));
        done_tx.send(()).expect("done");
    });
    assert!(done_rx.recv_timeout(Duration::from_millis(20)).is_err());
    drop(lease);
    done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("waiter woke");
    thread.join().expect("waiter");
}

#[test]
fn loss_revokes_inflight_generation_and_wakes_waiters() {
    let slot = group(&Arc::new(IrAllocator::new()));
    let lease = slot.acquire().expect("lease");
    let waiting = Arc::clone(&slot);
    let waiter = thread::spawn(move || waiting.acquire().err().expect("lost"));
    slot.lose("transport failed");
    drop(lease);

    assert_eq!(
        waiter.join().expect("waiter").to_string(),
        "transport failed"
    );
    assert_eq!(
        slot.acquire().err().expect("terminal loss").to_string(),
        "transport failed"
    );
}

#[test]
fn first_loss_reason_is_stable() {
    let slot = group(&Arc::new(IrAllocator::new()));
    slot.lose("socket detached");
    slot.lose("later cleanup failure");

    assert_eq!(
        slot.acquire().err().expect("lost").to_string(),
        "socket detached"
    );
}

#[test]
fn loss_wakes_every_waiter() {
    let slot = group(&Arc::new(IrAllocator::new()));
    let lease = slot.acquire().expect("lease");
    let waiters = (0..4)
        .map(|_| {
            let waiting = Arc::clone(&slot);
            thread::spawn(move || waiting.acquire().err().expect("lost").to_string())
        })
        .collect::<Vec<_>>();

    slot.lose("transport lost");
    drop(lease);
    for waiter in waiters {
        assert_eq!(waiter.join().expect("waiter"), "transport lost");
    }
}

#[test]
fn duplicate_context_add_preserves_original_state() {
    let slot = group(&Arc::new(IrAllocator::new()));
    let mut lease = slot.acquire().expect("group");
    let original = ContextState::with_version(3, 0, false);
    let duplicate = ContextState::with_version(2, 0, true);

    assert!(lease.data_mut().add(7, original));
    assert!(!lease.data_mut().add(7, duplicate));
    assert!(lease.data_mut().activate(7));
    assert_eq!(lease.data().gl.client_version(), (3, 0));
}

#[test]
fn independent_groups_cannot_mint_colliding_executor_names() {
    let allocator = Arc::new(IrAllocator::new());
    let first = group(&allocator);
    let second = group(&allocator);
    let first = first.acquire().expect("first");
    let second = second.acquire().expect("second");

    assert_eq!(first.data().gl.alloc_buffer_ir(), Ok(1));
    assert_eq!(second.data().gl.alloc_buffer_ir(), Ok(2));
    assert_eq!(first.data().gl.alloc_texture_ir(), Ok(1));
    assert_eq!(second.data().gl.alloc_texture_ir(), Ok(2));
}

struct BlockingSink {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

impl CommandSink for BlockingSink {
    fn negotiate(&mut self, _: &FeatureRequest) -> hl_gpu::Result<Capabilities> {
        Err(GpuError::Unsupported("test"))
    }

    fn submit(&mut self, _: &[Cmd]) -> hl_gpu::Result<()> {
        self.entered.send(()).expect("entered");
        self.release.recv().expect("released");
        Ok(())
    }

    fn wait(&mut self, _: FenceId, _: u64) -> hl_gpu::Result<()> {
        Err(GpuError::Unsupported("test"))
    }

    fn wait_timeout(&mut self, _: FenceId, _: u64, _: u64) -> hl_gpu::Result<FenceWait> {
        Err(GpuError::Unsupported("test"))
    }

    fn poll_fence(&mut self, _: FenceId, _: u64) -> hl_gpu::Result<bool> {
        Err(GpuError::Unsupported("test"))
    }

    fn read_buffer(&mut self, _: BufferId, _: u64, _: usize) -> hl_gpu::Result<Vec<u8>> {
        Err(GpuError::Unsupported("test"))
    }
}

#[test]
fn sibling_context_records_while_prepared_submit_waits_for_ack() {
    let slot = group(&Arc::new(IrAllocator::new()));
    let plan = {
        let mut lease = slot.acquire().expect("prepare lease");
        assert!(lease
            .data_mut()
            .add(1, ContextState::with_version(3, 0, false)));
        assert!(lease
            .data_mut()
            .add(2, ContextState::with_version(3, 0, false)));
        assert!(lease.data_mut().activate(1));
        SubmitPlan::prepare(|sink| sink.submit(&[Cmd::CreateFence(7)])).expect("prepared submit")
    };
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        plan.execute(&mut BlockingSink {
            entered: entered_tx,
            release: release_rx,
        })
    });
    entered_rx.recv().expect("submit entered");

    let mut sibling = slot.acquire().expect("sibling must not wait for ACK");
    assert!(sibling.data_mut().activate(2));
    sibling.data_mut().gl.set_gl_error(0x0501);
    drop(sibling);

    release_tx.send(()).expect("release submit");
    worker.join().expect("worker").expect("submit");
    let mut lease = slot.acquire().expect("collect");
    assert!(lease.data_mut().activate(2));
    assert_eq!(lease.data_mut().gl.take_gl_error(), 0x0501);
}
