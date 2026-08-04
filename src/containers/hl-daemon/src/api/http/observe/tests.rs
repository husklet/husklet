use super::{
    CpuTime, Options, ProcessColumn, ProcessMetrics, ProcessRow, StatsMode, TopOptions, docker_bool,
    sample_with_metrics, stats, stats_stream_response, supports_one_shot,
};
use axum::body::Body;
use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use hl_container::{Container, ContainerId, ContainerSpec, ContainerState, Process, Restart};
use std::{collections::BTreeMap, str::FromStr, sync::Arc};

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
fn stats_mode_selects_stream_one_shot_and_two_sample_behavior() {
    assert_eq!(
        Options::default().mode(false, true).unwrap(),
        StatsMode::Stream {
            stop_when_inactive: false
        }
    );
    assert_eq!(
        Options::default().mode(true, true).unwrap(),
        StatsMode::Stream {
            stop_when_inactive: true
        }
    );
    assert_eq!(
        Options {
            stream: Some("false".into()),
            one_shot: None,
        }
        .mode(false, true)
        .unwrap(),
        StatsMode::TwoSample
    );
    assert_eq!(
        Options {
            stream: Some("false".into()),
            one_shot: Some("true".into()),
        }
        .mode(true, true)
        .unwrap(),
        StatsMode::OneShot
    );
    assert_eq!(
        Options {
            stream: Some("true".into()),
            one_shot: Some("true".into()),
        }
        .mode(true, true),
        Err("cannot have stream=true and one-shot=true")
    );
}

#[test]
fn one_shot_is_gated_at_api_1_41() {
    for path in [
        "/containers/id/stats",
        "/v1.41/containers/id/stats",
        "/v1.43/containers/id/stats",
    ] {
        assert!(supports_one_shot(&path.parse().unwrap()), "{path}");
    }
    assert!(!supports_one_shot(&"/v1.40/containers/id/stats".parse().unwrap()));
    assert_eq!(
        Options {
            stream: Some("false".into()),
            one_shot: Some("true".into()),
        }
        .mode(true, false)
        .unwrap(),
        StatsMode::TwoSample
    );
}

#[tokio::test]
async fn stats_handler_rejects_streaming_one_shot_request() {
    let root = tempfile::tempdir().unwrap();
    let containers = hl_container::Containers::builder(
        hl_container::Config::new(root.path()).persistence(hl_container::Persistence::Memory),
    )
    .build()
    .await
    .unwrap();
    let container = containers
        .create(ContainerSpec::from_directory(root.path(), Process::new("/bin/true")).name("stats-options"))
        .await
        .unwrap();
    let state = super::super::DockerState {
        containers,
        platform: hl_images::Platform::linux_arm64(),
        source: Arc::new(hl_images::remote::Registry::new(hl_images::remote::Auth::Anonymous)),
        events: crate::events::Events::new(),
        builds: crate::builder::Builds::default(),
        release: crate::daemon::Release::default(),
        sampler: Arc::new(crate::process::UnavailableProcessSampler),
    };

    let error = stats(
        State(state),
        Path(container.id.to_string()),
        Query(Options {
            stream: Some("true".into()),
            one_shot: Some("true".into()),
        }),
        OriginalUri(
            "/v1.41/containers/stats-options/stats?stream=true&one-shot=true"
                .parse()
                .unwrap(),
        ),
    )
    .await
    .unwrap_err();

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
}

#[test]
fn stats_options_use_docker_boolean_query_semantics() {
    for value in ["", " 0 ", "NO", "False", "none"] {
        assert!(!docker_bool(Some(value), true), "{value:?}");
    }
    for value in ["1", "true", "yes", "off", "unexpected"] {
        assert!(docker_bool(Some(value), false), "{value:?}");
    }
    assert!(docker_bool(None, true));
    assert!(!docker_bool(None, false));
}

#[test]
fn stats_stream_response_explicitly_advertises_json() {
    let response = stats_stream_response(Body::empty());
    assert_eq!(response.headers().get(CONTENT_TYPE).unwrap(), "application/json");
}

#[test]
fn stopped_container_sample_does_not_fabricate_live_accounting() {
    let mut container = running();
    container.state = ContainerState::Created;

    let sample = ProcessMetrics::empty_sample(&container);
    assert_eq!(sample.read, "0001-01-01T00:00:00Z");
    assert_eq!(sample.pids_stats.current, 0);
    assert_eq!(sample.cpu_stats.online_cpus, 0);
    assert_eq!(sample.memory_stats.limit, 0);
}

#[test]
fn top_options_select_columns_and_reject_unknown_values() {
    let options = TopOptions {
        ps_args: Some("-eo pid,ppid,user,stat,args".into()),
        unsupported: BTreeMap::new(),
    };
    let columns = options.columns().unwrap();
    assert_eq!(
        columns.iter().map(|column| column.title()).collect::<Vec<_>>(),
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
fn top_custom_columns_require_pid_identity() {
    let unscoped = TopOptions {
        ps_args: Some("-eo user,args".into()),
        unsupported: BTreeMap::new(),
    };
    let error = match unscoped.columns() {
        Ok(_) => panic!("unscoped columns must fail"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(format!("{error:?}").contains("must include pid"));

    let scoped = TopOptions {
        ps_args: Some("-eo user,pid,args".into()),
        unsupported: BTreeMap::new(),
    };
    assert_eq!(
        scoped
            .columns()
            .unwrap()
            .iter()
            .map(|column| column.title())
            .collect::<Vec<_>>(),
        ["USER", "PID", "CMD"]
    );
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
        None,
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
        None,
    );
    assert_eq!(sample.memory_stats.usage, 12_345);
    assert_eq!(sample.cpu_stats.cpu_usage.total_usage, 67_890);
    assert_eq!(sample.precpu_stats.cpu_usage.total_usage, 0);
    assert_eq!(sample.preread, "0001-01-01T00:00:00Z");
}

#[test]
fn second_sample_projects_the_first_cpu_and_read_values() {
    let first = sample_with_metrics(
        &running(),
        Some(42),
        ProcessMetrics {
            memory: 12_000,
            cpu: CpuTime(0),
        },
        None,
    );
    let second = sample_with_metrics(
        &running(),
        Some(42),
        ProcessMetrics {
            memory: 12_345,
            cpu: CpuTime(67_890),
        },
        Some(&first),
    );

    assert_eq!(second.precpu_stats.cpu_usage.total_usage, 0);
    assert_eq!(second.precpu_stats, first.cpu_stats);
    assert_eq!(second.preread, first.read);
}
