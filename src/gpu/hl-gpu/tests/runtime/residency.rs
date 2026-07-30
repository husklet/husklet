use super::*;

/// Residency accounting: charge on create, refund exactly on destroy, and reject an over-connection-budget
/// create atomically (the failed charge never reaches the executor and never partially mutates the
/// ledger). Here `max_buffer_bytes` is large so the rejection is an ACCOUNTING (connection-budget) one,
/// not a per-object validation one.
#[test]
fn residency_charges_on_create_refunds_on_destroy_and_rejects_over_budget() {
    let caps = Capabilities::permissive_fixture("fake"); // large per-object ceilings
    let mut exec = FakeExecutor::new(caps.clone());
    // Connection budget: 4096 bytes / 8 objects.
    let mut limits = Limits::from_capabilities(caps);
    limits.max_connection_bytes = 4096;
    limits.max_connection_objects = 8;
    let mut s = session(limits, GlobalLedger::unbounded());

    // Charge on create.
    hl_gpu::runtime::submit(&mut s, &mut exec, 64, &[buffer(1, 4096)]).expect("exact fit");
    assert_eq!((s.residency_bytes(), s.object_count()), (4096, 1));

    // Over-budget create is rejected atomically at ACCOUNT — nothing charged, executor untouched.
    let executed_before = exec.command_count();
    let before_bytes = s.residency_bytes();
    let err = hl_gpu::runtime::submit(&mut s, &mut exec, 64, &[buffer(2, 1)]).unwrap_err();
    assert_eq!(err, GpuError::ResourceLimit("connection residency"));
    assert_eq!(
        s.residency_bytes(),
        before_bytes,
        "rejected charge did not mutate the ledger"
    );
    assert_eq!(s.object_count(), 1);
    assert_eq!(
        exec.command_count(),
        executed_before,
        "rejected charge never reached the executor"
    );

    // Refund on destroy, exactly — then the freed budget is reusable.
    hl_gpu::runtime::submit(&mut s, &mut exec, 32, &[Cmd::DestroyBuffer(1)])
        .expect("destroy refunds");
    assert_eq!((s.residency_bytes(), s.object_count()), (0, 0));
    hl_gpu::runtime::submit(&mut s, &mut exec, 64, &[buffer(3, 4096)])
        .expect("refunded budget reused");
    assert_eq!(s.residency_bytes(), 4096);
}

/// The shared global ledger isolates connections and a dropped connection refunds its whole global
/// contribution.
#[test]
fn global_ledger_isolates_connections_and_drop_refunds() {
    let caps = Capabilities::permissive_fixture("fake");
    let global = GlobalLedger::new(4096, 8);

    let mut e1 = FakeExecutor::new(caps.clone());
    let mut l1 = Limits::from_capabilities(caps.clone());
    l1.max_connection_bytes = 4096;
    let mut first = session(l1, global.clone());
    hl_gpu::runtime::submit(&mut first, &mut e1, 64, &[buffer(1, 4096)])
        .expect("first fills global");

    // A second connection cannot allocate past the shared global byte ceiling.
    let mut e2 = FakeExecutor::new(caps.clone());
    let mut l2 = Limits::from_capabilities(caps);
    l2.max_connection_bytes = 4096;
    let mut second = session(l2, global.clone());
    assert_eq!(
        hl_gpu::runtime::submit(&mut second, &mut e2, 64, &[buffer(9, 4096)]).unwrap_err(),
        GpuError::ResourceLimit("global residency")
    );

    // Dropping the first connection refunds the global account so the second now fits.
    drop(first);
    hl_gpu::runtime::submit(&mut second, &mut e2, 64, &[buffer(9, 4096)])
        .expect("disconnect refunded the global owner");
}
