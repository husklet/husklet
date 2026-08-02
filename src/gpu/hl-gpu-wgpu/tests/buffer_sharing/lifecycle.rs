use super::*;
#[test]
fn nonpayer_disconnect_discards_its_reservation_without_changing_global_charge() {
    let device = Device::new(DeviceConfig::default()).expect("Metal adapter");
    let mut owner_exec = device.executor();
    let first_exec = device.executor();
    let second_exec = device.executor();
    let exports = Exports::new();
    let global = GlobalLedger::new(1024, 16);
    let make = |exec: &hl_gpu_wgpu::WgpuExecutor| {
        Session::new(
            Limits::from_capabilities(exec.capabilities()),
            global.clone(),
            Box::new(FakeClock::new(0)),
        )
        .with_exports(exports.clone())
    };
    let mut owner = make(&owner_exec);
    let mut first = make(&first_exec);
    let mut second = make(&second_exec);
    let desc = BufferDesc {
        size: 4,
        usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
        label: String::new(),
    };
    hl_gpu::runtime::submit(
        &mut owner,
        &mut owner_exec,
        0,
        &[Cmd::CreateBuffer(1, desc)],
    )
    .unwrap();
    let export =
        hl_gpu::runtime::service::dispatch::export_buffer(&mut owner, &owner_exec, BufferId(1))
            .unwrap();
    hl_gpu::runtime::service::dispatch::import_buffer(&mut first, &first_exec, BufferId(2), export)
        .unwrap();
    hl_gpu::runtime::service::dispatch::import_buffer(
        &mut second,
        &second_exec,
        BufferId(3),
        export,
    )
    .unwrap();
    hl_gpu::runtime::submit(&mut owner, &mut owner_exec, 0, &[Cmd::DestroyBuffer(1)]).unwrap();
    assert_eq!(second.account.reserved_bytes(), 4);
    drop(second);
    assert_eq!(global.residency_bytes(), 4);
    assert_eq!(first.account.reserved_bytes(), 0);
    first.release_all();
    assert_eq!(global.residency_bytes(), 0);
    assert!(!exports.is_live(export));
}

fn active_payer_recreates_same_id(replacement_bytes: u64) {
    let device = Device::new(DeviceConfig::default()).expect("Metal adapter");
    let mut owner_exec = device.executor();
    let mut payer_exec = device.executor();
    let next_exec = device.executor();
    let exports = Exports::new();
    let global = GlobalLedger::new(4096, 32);
    let make = |exec: &hl_gpu_wgpu::WgpuExecutor| {
        Session::new(
            Limits::from_capabilities(exec.capabilities()),
            global.clone(),
            Box::new(FakeClock::new(0)),
        )
        .with_exports(exports.clone())
    };
    let mut owner = make(&owner_exec);
    let mut payer = make(&payer_exec);
    let mut next = make(&next_exec);
    let old = BufferDesc {
        size: 8,
        usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
        label: String::new(),
    };
    hl_gpu::runtime::submit(&mut owner, &mut owner_exec, 0, &[Cmd::CreateBuffer(1, old)]).unwrap();
    let export =
        hl_gpu::runtime::service::dispatch::export_buffer(&mut owner, &owner_exec, BufferId(1))
            .unwrap();
    hl_gpu::runtime::service::dispatch::import_buffer(&mut payer, &payer_exec, BufferId(2), export)
        .unwrap();
    hl_gpu::runtime::service::dispatch::import_buffer(&mut next, &next_exec, BufferId(3), export)
        .unwrap();
    hl_gpu::runtime::submit(&mut owner, &mut owner_exec, 0, &[Cmd::DestroyBuffer(1)]).unwrap();
    let replacement = BufferDesc {
        size: replacement_bytes,
        usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
        label: String::new(),
    };
    hl_gpu::runtime::submit(
        &mut payer,
        &mut payer_exec,
        0,
        &[Cmd::DestroyBuffer(2), Cmd::CreateBuffer(2, replacement)],
    )
    .unwrap();
    assert_eq!(
        payer
            .account
            .ledger()
            .live
            .get(&(hl_gpu::runtime::KIND_BUFFER, 2)),
        Some(&replacement_bytes)
    );
    assert_eq!(payer.account.ledger().totals.bytes, replacement_bytes);
    assert_eq!(next.account.ledger().totals.bytes, 8);
    assert_eq!(global.residency_bytes(), replacement_bytes + 8);
    payer.release_all();
    next.release_all();
    assert_eq!(global.residency_bytes(), 0);
}

#[test]
fn active_payer_same_id_larger_replacement_keeps_exact_ledger() {
    active_payer_recreates_same_id(32);
}

#[test]
fn active_payer_same_id_smaller_replacement_keeps_exact_ledger() {
    active_payer_recreates_same_id(4);
}
