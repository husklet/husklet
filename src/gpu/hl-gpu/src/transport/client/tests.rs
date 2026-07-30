use super::*;
use crate::protocol::model::descriptor::BufferDesc;
use crate::protocol::model::enums::buffer_usage;

#[test]
fn residency_over_replay_budget_reports_clean_api_loss() {
    // When acknowledged residency exceeds the channel's replay budget, a reconnect must report a
    // clean, typed API loss instead of silently recovering a truncated set.
    let mk = |id| {
        Cmd::CreateBuffer(
            id,
            BufferDesc {
                size: 16,
                usage: buffer_usage::COPY_DST,
                label: String::new(),
            },
        )
    };
    let mut journal = ResidencyJournal::with_budget(30);
    journal.append(&[mk(1)]);
    assert!(
        journal.replay_bytes().is_ok(),
        "residency within budget replays"
    );
    journal.append(&[mk(2)]); // pushes the encoded journal past the replay budget
    let err = journal
        .replay_bytes()
        .expect_err("over-budget residency must not silently truncate");
    assert!(matches!(err, TransportError::ApiLost { .. }));
}

#[test]
fn residency_replay_loss_poison_is_stable() {
    let (client, _server) = UnixStream::pair().unwrap();
    let mut sink = RemoteCommandSink::new("unused");
    sink.sock = Some(client);
    sink.residency_reset = true;
    sink.residency = ResidencyJournal::with_budget(1);
    sink.residency.append(&[Cmd::CreateFence(1)]);

    assert!(matches!(
        sink.submit_ir(&[], &[], 0),
        Err(GpuError::Transport(TransportError::ApiLost { .. }))
    ));
    assert!(matches!(
        sink.submit_ir(&[], &[], 0),
        Err(GpuError::Transport(TransportError::Poisoned { .. }))
    ));
}

#[test]
fn capability_change_with_live_residency_is_typed_api_loss() {
    let mut conn = RemoteCommandSink::new("unused");
    let caps = Capabilities::full("host");
    conn.set_negotiated_capabilities(&caps).unwrap();
    conn.residency.append(&[Cmd::CreateFence(1)]);

    let mut changed = caps;
    changed.wire_version += 1;
    let err = conn
        .set_negotiated_capabilities(&changed)
        .expect_err("live profile change is loss");
    assert!(matches!(
        err,
        GpuError::Transport(TransportError::ApiLost { .. })
    ));
    assert!(matches!(
        conn.connect(),
        Err(GpuError::Transport(TransportError::Poisoned { .. }))
    ));
}

#[test]
fn residency_skips_presents_and_waits() {
    let mut journal = ResidencyJournal::default();
    journal.append(&[
        Cmd::CreateFence(1),
        Cmd::Present {
            surface: 1,
            texture: 2,
            serial: crate::FrameSerial::new(3).unwrap(),
        },
        Cmd::WaitFence { id: 1, value: 3 },
    ]);
    // Only the create is residency; present/wait are observations.
    assert_eq!(journal.cmds, vec![Cmd::CreateFence(1)]);
}

fn buf(id: u32) -> Cmd {
    Cmd::CreateBuffer(
        id,
        BufferDesc {
            size: 64,
            usage: buffer_usage::VERTEX,
            label: String::new(),
        },
    )
}

fn submit_refs(buffers: &[u32]) -> Cmd {
    use crate::protocol::model::command::{CommandBuffer, Enc};
    use crate::protocol::model::enums::IndexFormat;
    let encoder = buffers
        .iter()
        .map(|&b| Enc::SetVertexBuffer {
            slot: 0,
            buffer: b,
            offset: 0,
        })
        .chain(std::iter::once(Enc::SetIndexBuffer {
            buffer: buffers[0],
            offset: 0,
            format: IndexFormat::U16,
        }))
        .collect();
    Cmd::Submit(CommandBuffer {
        encoder,
        signal: None,
    })
}

#[test]
fn teardown_destroys_compact_the_dead_working_set_out_of_the_journal() {
    // A whole working set (creates + a submit that uses it) then fully destroyed — the lost-context
    // teardown pattern. After the destroys the journal must hold NOTHING: a reconnect would otherwise
    // replay every dead resource's create and re-inflate the host ledger.
    let mut journal = ResidencyJournal::default();
    journal.append(&[buf(1), buf(2), submit_refs(&[1, 2])]);
    assert!(
        !journal.cmds.is_empty() && journal.bytes > 0,
        "working set recorded"
    );

    journal.append(&[Cmd::DestroyBuffer(1), Cmd::DestroyBuffer(2)]);
    assert!(
        journal.cmds.is_empty(),
        "a fully torn-down working set leaves the journal empty"
    );
    assert_eq!(
        journal.bytes, 0,
        "compacted journal reports zero live residency"
    );
    assert!(
        journal.replay_bytes().is_ok(),
        "an empty journal replays cleanly"
    );
}

#[test]
fn compaction_keeps_live_residency_and_drops_only_the_dead() {
    // Two independent resources: buf 1 stays LIVE (used by submit A, never destroyed); buf 2 is created,
    // used by its OWN submit B, then destroyed. Only buf 2's create + submit B may leave the journal;
    // buf 1's create + submit A must survive so a reconnect still rebuilds the live resource.
    let mut journal = ResidencyJournal::default();
    journal.append(&[buf(1), buf(2), submit_refs(&[1]), submit_refs(&[2])]);
    journal.append(&[Cmd::DestroyBuffer(2)]);

    assert!(
        journal.cmds.contains(&buf(1)),
        "the live resource's create survives"
    );
    assert!(
        journal.cmds.contains(&submit_refs(&[1])),
        "the live resource's submit survives"
    );
    assert!(
        !journal.cmds.contains(&buf(2)),
        "the dead resource's create is compacted out"
    );
    assert!(
        !journal.cmds.contains(&submit_refs(&[2])),
        "the dead resource's submit is compacted out"
    );
    assert!(
        !journal
            .cmds
            .iter()
            .any(|c| matches!(c, Cmd::DestroyBuffer(2))),
        "its destroy too"
    );
}

#[test]
fn compaction_reclaims_budget_so_churn_does_not_falsely_trip_replay_loss() {
    // A churny client that repeatedly creates + destroys a working set must NOT trip the replay budget:
    // the LIVE residency stays tiny even though the cumulative history is large. Compaction keeps the
    // journal bounded to the live set, so replay stays available.
    let mut journal = ResidencyJournal::with_budget(4096);
    for generation in 0..500u32 {
        let a = generation * 2 + 1;
        let b = generation * 2 + 2;
        journal.append(&[buf(a), buf(b), submit_refs(&[a, b])]);
        journal.append(&[Cmd::DestroyBuffer(a), Cmd::DestroyBuffer(b)]);
    }
    assert!(
        journal.cmds.is_empty(),
        "no live residency remains after balanced create/destroy churn"
    );
    assert!(
        journal.replay_bytes().is_ok(),
        "compaction kept the journal replayable (no false API loss)"
    );
}
