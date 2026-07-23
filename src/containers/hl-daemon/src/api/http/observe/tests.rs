use super::{sample_with_metrics, CpuTime, ProcessColumn, ProcessMetrics, ProcessRow, TopOptions};
use hl_container::{Container, ContainerId, ContainerSpec, ContainerState, Process, Restart};
use std::{collections::BTreeMap, str::FromStr};

fn running() -> Container {
    Container {
        id: ContainerId::from_str("00000000000000000000000000000000").unwrap(),
        spec: ContainerSpec::from_directory(".", Process::new("/bin/true")),
        state: ContainerState::Running {
            process_id: 42,
            started_at_ms: 1,
        },
        created_at_ms: 1,
        generation: 0,
        restart: Restart::default(),
        health: None,
        checkpoint: None,
    }
}

#[test]
fn top_options_select_columns_and_reject_unknown_values() {
    let options = TopOptions {
        ps_args: Some("-eo pid,ppid,user,stat,args".into()),
        unsupported: BTreeMap::new(),
    };
    let columns = options.columns().unwrap();
    assert_eq!(
        columns
            .iter()
            .map(|column| column.title())
            .collect::<Vec<_>>(),
        ["PID", "PPID", "USER", "STAT", "CMD"]
    );
    let invalid = TopOptions {
        ps_args: Some("-eo pid,nsenter".into()),
        unsupported: BTreeMap::new(),
    };
    assert!(invalid.columns().is_err());
    let unknown = TopOptions {
        ps_args: None,
        unsupported: BTreeMap::from([("watch".into(), "true".into())]),
    };
    assert!(unknown.columns().is_err());
    assert_eq!(ProcessColumn::DEFAULT[3].title(), "C");
}

#[test]
fn top_does_not_fabricate_unmeasured_process_accounting() {
    let row = ProcessRow {
        user: "root".into(),
        terminal: "?".into(),
        state: "?".into(),
        command: "/bin/sleep 60".into(),
        name: "/bin/sleep".into(),
    };
    for column in [
        ProcessColumn::Cpu,
        ProcessColumn::CpuPercent,
        ProcessColumn::Memory,
        ProcessColumn::VirtualSize,
        ProcessColumn::ResidentSize,
        ProcessColumn::Start,
        ProcessColumn::Time,
    ] {
        assert_eq!(row.value(column), "?");
    }
}

#[test]
fn unavailable_measurements_are_not_replaced_with_synthetic_activity() {
    let sample = sample_with_metrics(
        &running(),
        Some(42),
        ProcessMetrics {
            memory: 0,
            cpu: CpuTime(0),
        },
        0,
    );
    assert_eq!(sample.memory_stats.usage, 0);
    assert_eq!(sample.cpu_stats.cpu_usage.total_usage, 0);
    assert_eq!(sample.cpu_stats.system_cpu_usage, 0);
    assert_eq!(sample.precpu_stats.cpu_usage.total_usage, 0);
}

#[test]
fn measured_process_values_are_preserved_exactly() {
    let sample = sample_with_metrics(
        &running(),
        Some(42),
        ProcessMetrics {
            memory: 12_345,
            cpu: CpuTime(67_890),
        },
        12_000,
    );
    assert_eq!(sample.memory_stats.usage, 12_345);
    assert_eq!(sample.cpu_stats.cpu_usage.total_usage, 67_890);
    assert_eq!(sample.precpu_stats.cpu_usage.total_usage, 12_000);
}

#[test]
fn cpu_times_accept_ps_formats() {
    assert_eq!(CpuTime::from("01:23").0, 83_000_000_000);
    assert_eq!(CpuTime::from("01:00:00").0, 3_600_000_000_000);
    assert_eq!(CpuTime::from("2-00:00:00").0, 172_800_000_000_000);
    assert_eq!(CpuTime::from("00:09.99").0, 9_000_000_000);
}
