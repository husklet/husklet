use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use hl_container::{Container, ContainerState};
use serde::Deserialize;
use std::collections::BTreeMap;

use super::error::{ApiError, ApiResult};
use super::DockerState;
use crate::api::{BlockIo, Cpu, CpuUsage, Memory, Pids, Stats, Throttling, Top};

const DEFAULT_MEMORY: u64 = 8 * 1024 * 1024 * 1024;

#[hl_design::adapter]
pub(super) async fn top(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(options): Query<TopOptions>,
) -> ApiResult<Json<Top>> {
    let columns = options.columns()?;
    let container = state
        .containers
        .inspect(&id)
        .await
        .map_err(ApiError::container)?;
    if !matches!(
        container.state,
        ContainerState::Running { .. } | ContainerState::Paused { .. }
    ) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!(
                "Container {} is not running",
                &container.id.as_str()[..container.id.as_str().len().min(12)]
            ),
        ));
    }
    let process = ProcessRow::new(&container);
    Ok(Json(Top {
        titles: columns.iter().map(|column| column.title().into()).collect(),
        processes: vec![columns
            .iter()
            .map(|column| process.value(*column))
            .collect()],
    }))
}

#[derive(Default, Deserialize)]
pub(super) struct TopOptions {
    ps_args: Option<String>,
    #[serde(flatten)]
    unsupported: BTreeMap<String, String>,
}

impl TopOptions {
    fn columns(&self) -> ApiResult<Vec<ProcessColumn>> {
        if let Some(name) = self.unsupported.keys().next() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("unsupported top option {name:?}"),
            ));
        }
        let value = self.ps_args.as_deref().unwrap_or("-ef").trim();
        if value.is_empty() || value == "-ef" {
            return Ok(ProcessColumn::DEFAULT.to_vec());
        }
        if value == "aux" {
            return Ok(ProcessColumn::AUX.to_vec());
        }
        let fields = value
            .strip_prefix("-eo ")
            .or_else(|| value.strip_prefix("-o "))
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("unsupported ps_args {value:?}; expected -ef, aux, -o, or -eo"),
                )
            })?;
        let columns = fields
            .split(',')
            .map(str::trim)
            .map(ProcessColumn::parse)
            .collect::<ApiResult<Vec<_>>>()?;
        if columns.is_empty() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "top column list cannot be empty",
            ));
        }
        Ok(columns)
    }
}

#[derive(Clone, Copy)]
enum ProcessColumn {
    User,
    Uid,
    Pid,
    ParentPid,
    Cpu,
    CpuPercent,
    Memory,
    VirtualSize,
    ResidentSize,
    State,
    Start,
    Terminal,
    Time,
    Command,
    Name,
}

impl ProcessColumn {
    const DEFAULT: [Self; 8] = [
        Self::Uid,
        Self::Pid,
        Self::ParentPid,
        Self::Cpu,
        Self::Start,
        Self::Terminal,
        Self::Time,
        Self::Command,
    ];
    const AUX: [Self; 11] = [
        Self::User,
        Self::Pid,
        Self::CpuPercent,
        Self::Memory,
        Self::VirtualSize,
        Self::ResidentSize,
        Self::Terminal,
        Self::State,
        Self::Start,
        Self::Time,
        Self::Command,
    ];

    fn parse(value: &str) -> ApiResult<Self> {
        match value.to_ascii_lowercase().as_str() {
            "user" => Ok(Self::User),
            "uid" => Ok(Self::Uid),
            "pid" => Ok(Self::Pid),
            "ppid" => Ok(Self::ParentPid),
            "c" => Ok(Self::Cpu),
            "%cpu" | "pcpu" => Ok(Self::CpuPercent),
            "%mem" | "pmem" => Ok(Self::Memory),
            "vsz" => Ok(Self::VirtualSize),
            "rss" => Ok(Self::ResidentSize),
            "stat" | "state" => Ok(Self::State),
            "stime" | "start" => Ok(Self::Start),
            "tty" => Ok(Self::Terminal),
            "time" => Ok(Self::Time),
            "args" | "cmd" | "command" => Ok(Self::Command),
            "comm" => Ok(Self::Name),
            _ => Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("unsupported top column {value:?}"),
            )),
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::User => "USER",
            Self::Uid => "UID",
            Self::Pid => "PID",
            Self::ParentPid => "PPID",
            Self::Cpu => "C",
            Self::CpuPercent => "%CPU",
            Self::Memory => "%MEM",
            Self::VirtualSize => "VSZ",
            Self::ResidentSize => "RSS",
            Self::State => "STAT",
            Self::Start => "STIME",
            Self::Terminal => "TTY",
            Self::Time => "TIME",
            Self::Command => "CMD",
            Self::Name => "COMMAND",
        }
    }
}

struct ProcessRow {
    user: String,
    terminal: String,
    state: String,
    command: String,
    name: String,
}

impl ProcessRow {
    fn new(container: &Container) -> Self {
        let mut command = container.spec.process.program.clone();
        for argument in &container.spec.process.args {
            command.push(' ');
            command.push_str(argument);
        }
        Self {
            user: container
                .spec
                .process
                .uid
                .map_or_else(|| "root".into(), |value| value.to_string()),
            terminal: container
                .spec
                .process
                .console
                .terminal
                .map_or_else(|| "?".into(), |_| "/dev/pts/0".into()),
            state: if matches!(container.state, ContainerState::Paused { .. }) {
                "T".into()
            } else {
                "?".into()
            },
            name: container.spec.process.program.clone(),
            command,
        }
    }

    fn value(&self, column: ProcessColumn) -> String {
        match column {
            ProcessColumn::User | ProcessColumn::Uid => self.user.clone(),
            ProcessColumn::Pid => "1".into(),
            ProcessColumn::ParentPid => "0".into(),
            ProcessColumn::Cpu
            | ProcessColumn::VirtualSize
            | ProcessColumn::ResidentSize
            | ProcessColumn::CpuPercent
            | ProcessColumn::Memory
            | ProcessColumn::Start
            | ProcessColumn::Time => "?".into(),
            ProcessColumn::State => self.state.clone(),
            ProcessColumn::Terminal => self.terminal.clone(),
            ProcessColumn::Command => self.command.clone(),
            ProcessColumn::Name => self.name.clone(),
        }
    }
}

#[derive(Default, Deserialize)]
pub(super) struct Options {
    stream: Option<String>,
}

impl Options {
    fn streams(&self) -> bool {
        !matches!(
            self.stream.as_deref(),
            Some("0" | "false" | "False" | "no" | "off")
        )
    }
}

#[hl_design::adapter]
pub(super) async fn stats(
    State(state): State<DockerState>,
    Path(id): Path<String>,
    Query(options): Query<Options>,
) -> ApiResult<Response> {
    let container = state
        .containers
        .inspect(&id)
        .await
        .map_err(ApiError::container)?;
    if !options.streams() || !container.state.is_active() {
        return Ok(Json(ProcessMetrics::sample(&container, 0)).into_response());
    }
    let containers = state.containers.clone();
    let reference = container.id.to_string();
    let body = futures_util::stream::unfold((0_u64, 0_u64), move |(index, previous)| {
        let containers = containers.clone();
        let reference = reference.clone();
        async move {
            if index > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            let container = containers.inspect(&reference).await.ok()?;
            if !container.state.is_active() {
                return None;
            }
            let value = ProcessMetrics::sample(&container, previous);
            let next = (index + 1, value.cpu_stats.cpu_usage.total_usage);
            let mut bytes = serde_json::to_vec(&value).ok()?;
            bytes.push(b'\n');
            Some((Ok::<_, std::io::Error>(bytes), next))
        }
    });
    Ok(Response::builder()
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(Body::from_stream(body))
        .expect("static stats response is valid"))
}

fn sample_with_metrics(
    container: &Container,
    pid: Option<u64>,
    metrics: ProcessMetrics,
    previous: u64,
) -> Stats {
    let (usage, total) = (metrics.memory, metrics.cpu.0);
    let previous = if previous == 0 { total } else { previous };
    let cpu = |total, system| Cpu {
        cpu_usage: CpuUsage {
            total_usage: total,
            usage_in_kernelmode: 0,
            usage_in_usermode: total,
        },
        system_cpu_usage: system,
        online_cpus: container.spec.resources.cpu_count.max(1),
        throttling_data: Throttling {
            periods: 0,
            throttled_periods: 0,
            throttled_time: 0,
        },
    };
    let current = u64::from(pid.is_some());
    Stats {
        read: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
        preread: "0001-01-01T00:00:00Z".into(),
        name: format!(
            "/{}",
            container.spec.name.as_deref().unwrap_or_else(|| {
                &container.id.as_str()[..container.id.as_str().len().min(12)]
            })
        ),
        id: container.id.to_string(),
        pids_stats: Pids { current },
        // The engine does not yet expose host-wide CPU accounting. Zero means unavailable; inventing a
        // wall-clock denominator makes idle containers appear busy.
        cpu_stats: cpu(total, 0),
        precpu_stats: cpu(previous, 0),
        memory_stats: Memory {
            usage,
            max_usage: usage,
            limit: match container.spec.resources.memory_bytes {
                0 => DEFAULT_MEMORY,
                value => value,
            },
            failcnt: 0,
            stats: BTreeMap::new(),
        },
        blkio_stats: BlockIo::empty(),
        networks: BTreeMap::new(),
        num_procs: u32::try_from(current).unwrap_or_default(),
        storage_stats: BTreeMap::new(),
    }
}

#[derive(Clone, Copy, Default)]
struct ProcessMetrics {
    memory: u64,
    cpu: CpuTime,
}

impl ProcessMetrics {
    fn sample(container: &Container, previous: u64) -> Stats {
        let pid = match &container.state {
            ContainerState::Running { process_id, .. }
            | ContainerState::Paused { process_id, .. } => Some(*process_id),
            _ => None,
        };
        sample_with_metrics(
            container,
            pid,
            pid.map_or_else(Self::default, Self::read),
            previous,
        )
    }

    fn read(process_id: u64) -> Self {
        let sample = crate::adapter::process::Sample::read(process_id);
        Self {
            memory: sample.memory,
            cpu: CpuTime(sample.cpu_seconds.saturating_mul(1_000_000_000)),
        }
    }
}

#[derive(Clone, Copy, Default)]
struct CpuTime(u64);

#[cfg(test)]
mod tests;
