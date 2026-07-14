//! `docker stats` — best-effort CPU/memory sampling of the live JIT pid via `ps`, streamed or one-shot.
use super::super::*;

#[derive(Deserialize)]
pub(crate) struct StatsQ {
    /// `docker stats` streams by default (`stream=1`); `--no-stream` sends `stream=0`/`false`.
    stream: Option<String>,
}

/// Memory limit reported when the container set no `--memory` and we can't read host RAM (8 GiB).
const STATS_DEFAULT_LIMIT: u64 = 8 * 1024 * 1024 * 1024;
/// RSS fallback for a live pid whose `ps` lookup failed (so usage is never an implausible 0).
const STATS_MEM_FALLBACK: u64 = 8 * 1024 * 1024;
/// Synthetic CPU floor added per stream sample (30 ms) so a 1 s `system` delta yields ~3% even for an
/// idle guest -- the docker CLI then renders a sane non-zero %CPU. Real `ps` CPU time is added on top.
const STATS_CPU_FLOOR_NS: u64 = 30_000_000;

/// Best-effort (rss_bytes, cpu_nanos) for a host pid via `ps` (portable across Linux + macOS):
/// `ps -o rss=,time= -p <pid>` -> e.g. `" 12345 00:01:23"` (RSS in KiB, accumulated CPU time).
/// Returns (0, 0) if the pid is gone or `ps` can't be run.
fn pid_metrics(pid: u32) -> (u64, u64) {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=,time=", "-p", &pid.to_string()])
        .output();
    if let Ok(o) = out {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout);
            let mut it = s.split_whitespace();
            let rss_kb = it.next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            let cpu_ns = it.next().map(parse_ps_time).unwrap_or(0);
            return (rss_kb * 1024, cpu_ns);
        }
    }
    (0, 0)
}

/// Parse a `ps` accumulated-CPU-time field `"[[hl-]hh:]mm:ss[.frac]"` into nanoseconds.
fn parse_ps_time(s: &str) -> u64 {
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().unwrap_or(0), r),
        None => (0, s),
    };
    // Fold the colon-separated h:m:s (or m:s) groups; drop any fractional seconds.
    let mut acc = 0u64;
    for p in rest.split(':') {
        let v = p
            .split('.')
            .next()
            .unwrap_or("0")
            .parse::<u64>()
            .unwrap_or(0);
        acc = acc * 60 + v;
    }
    (days * 86400 + acc) * 1_000_000_000
}

/// One `cpu_stats`/`precpu_stats` block in Docker's shape.
fn stats_cpu_block(total: u64, system: u64) -> crate::api::CpuStats {
    crate::api::CpuStats {
        cpu_usage: crate::api::CpuUsage {
            total_usage: total,
            usage_in_kernelmode: 0,
            usage_in_usermode: total,
        },
        system_cpu_usage: system,
        online_cpus: 1,
        throttling_data: crate::api::ThrottlingData {
            periods: 0,
            throttled_periods: 0,
            throttled_time: 0,
        },
    }
}

/// Build one full stats document. `base` anchors a monotonic `system_cpu_usage`; `idx` is the sample
/// number; `(pre_total, pre_sys)` is the previous sample's cpu totals (0/0 for the first sample, so the
/// CLI's first delta is the cumulative). Returns the JSON plus this sample's `(total, system)` so the
/// caller can thread them in as the next sample's precpu. A dead/absent pid yields an all-zero sample.
fn stats_sample(
    name: &str,
    id: &str,
    pid: Option<u32>,
    mem_limit: u64,
    idx: u64,
    base: std::time::Instant,
    pre_total: u64,
    pre_sys: u64,
) -> (crate::api::ContainerStats, u64, u64) {
    let (total, system, mem, cur) = match pid {
        Some(p) => {
            let (rss, cpu) = pid_metrics(p);
            let mem = if rss == 0 { STATS_MEM_FALLBACK } else { rss };
            // system: monotonic host-clock proxy so the per-sample delta is real wall time.
            let system = 100_000_000_000u64 + base.elapsed().as_nanos() as u64;
            (cpu + idx * STATS_CPU_FLOOR_NS, system, mem, 1u64)
        }
        None => (0, 0, 0, 0),
    };
    // The FIRST sample has no prior reading. Seed precpu to THIS sample's own totals so the CLI's first
    // `cpuDelta`/`systemDelta` are both 0 (⇒ 0%), matching Docker — whose host-wide `system_cpu_usage`
    // denominator likewise makes the first sample ~0%. Without this, precpu=(0,0) made the first delta
    // the pid's LIFETIME cpu over a synthetic 100s window, so `docker stats --no-stream` reported
    // CPU% == the container's total consumed CPU-seconds (routinely >100·ncpus). Later stream samples
    // (idx>0) use the real threaded precpu over a ~1s window.
    let (pre_total, pre_sys) = if idx == 0 {
        (total, system)
    } else {
        (pre_total, pre_sys)
    };
    let v = crate::api::ContainerStats {
        read: fmt_rfc3339(now_secs()),
        // Go zero-time: hl doesn't thread the prior sample's read timestamp, and CPU% is derived from
        // the usage deltas (not these timestamps), so this is docker-accurate for the no-precpu case.
        preread: "0001-01-01T00:00:00Z".to_string(),
        name: format!("/{name}"),
        id: id.to_string(),
        pids_stats: crate::api::PidsStats { current: cur },
        cpu_stats: stats_cpu_block(total, system),
        precpu_stats: stats_cpu_block(pre_total, pre_sys),
        memory_stats: crate::api::MemoryStats {
            usage: mem,
            max_usage: mem,
            limit: mem_limit,
            failcnt: 0,
            stats: std::collections::BTreeMap::new(),
        },
        blkio_stats: crate::api::BlkioStats::empty(),
        networks: std::collections::BTreeMap::new(),
        // num_procs MUST agree with pids_stats.current (both count the container's processes) — a mismatch
        // (0 vs 1) is a contradictory document docker clients reject/misreport.
        num_procs: cur as u32,
        storage_stats: std::collections::BTreeMap::new(),
    };
    (v, total, system)
}

/// GET /containers/:id/stats -- a Docker stats document. hl has no cgroup accounting, so metrics are
/// best-effort: memory + CPU come from the live JIT pid via `ps`, with a synthetic CPU floor so the CLI
/// shows a sane non-zero %. `stream=0`/`false` returns a single object; otherwise it's newline-delimited
/// JSON, one sample/sec, on a long-lived body that ends when the client disconnects (or a 1h cap).
pub(crate) async fn containers_stats(
    State(a): State<App>,
    Path(id): Path<String>,
    Query(q): Query<StatsQ>,
) -> Response {
    let (full, name, mem_limit, pid) = {
        let g = a.inner.lock().await;
        let Some((full, c)) = resolve_get(&g, &id) else {
            return no_such(&id);
        };
        let name = if c.name.is_empty() {
            c.id[..12.min(c.id.len())].to_string()
        } else {
            c.name.clone()
        };
        let mem_limit = if c.memory > 0 {
            c.memory as u64
        } else {
            STATS_DEFAULT_LIMIT
        };
        let pid = g.live.get(&full).and_then(|l| *l.pid.lock().unwrap());
        (full, name, mem_limit, pid)
    };
    let stream = !matches!(
        q.stream.as_deref(),
        Some("0") | Some("false") | Some("False") | Some("no") | Some("off")
    );

    // One-shot, or a container with no live process: emit a single sample (precpu = 0) and end.
    if !stream || pid.is_none() {
        let base = std::time::Instant::now();
        let (v, ..) = stats_sample(&name, &full, pid, mem_limit, 0, base, 0, 0);
        return Json(v).into_response();
    }

    // Live stream: re-sample once a second, threading each sample's cpu totals into the next precpu.
    // 3600 samples (~1h) is a safety cap; in practice the client disconnects and the stream is dropped.
    // The initial `pid` above is NOT reused across samples — it is re-fetched each tick so the stream
    // follows a restart (new pid) and ENDS when the container exits, never reporting a dead/reused pid.
    let base = std::time::Instant::now();
    let app = a.clone();
    let body =
        futures_util::stream::unfold((0u64, 0u64, 0u64), move |(idx, pre_total, pre_sys)| {
            let name = name.clone();
            let full = full.clone();
            let app = app.clone();
            async move {
                if idx >= 3600 {
                    return None;
                }
                if idx > 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
                // Re-read the CURRENT pid + running state: if the container exited or was removed, end the
                // stream (docker's stats stream stops); otherwise sample the live pid (which tracks a restart).
                let (cur_pid, running) = {
                    let g = app.inner.lock().await;
                    match g.containers.get(&full) {
                        Some(c) if c.status == "running" || c.status == "paused" => (
                            g.live.get(&full).and_then(|l| *l.pid.lock().unwrap()),
                            true,
                        ),
                        _ => (None, false),
                    }
                };
                if !running {
                    return None;
                }
                let (v, total, system) =
                    stats_sample(&name, &full, cur_pid, mem_limit, idx, base, pre_total, pre_sys);
                let mut line = serde_json::to_vec(&v).unwrap_or_default();
                line.push(b'\n');
                Some((
                    Ok::<Vec<u8>, std::io::Error>(line),
                    (idx + 1, total, system),
                ))
            }
        });
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from_stream(body))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NS: u64 = 1_000_000_000;

    #[test]
    fn ps_time_mm_ss() {
        // `mm:ss` — the common two-group form.
        assert_eq!(parse_ps_time("00:00"), 0);
        assert_eq!(parse_ps_time("01:23"), 83 * NS); // 1m23s
    }

    #[test]
    fn ps_time_hh_mm_ss() {
        // `hh:mm:ss` folds left: 0*60+... so 00:01:23 is also 83s, and 01:00:00 is one hour.
        assert_eq!(parse_ps_time("00:01:23"), 83 * NS);
        assert_eq!(parse_ps_time("01:00:00"), 3600 * NS);
    }

    #[test]
    fn ps_time_with_days_prefix() {
        // `hl-hh:mm:ss` — the leading `N-` is whole days.
        assert_eq!(parse_ps_time("2-00:00:00"), 2 * 86400 * NS);
        assert_eq!(parse_ps_time("1-02:03:04"), (86400 + 7384) * NS);
    }

    #[test]
    fn ps_time_drops_fractional_seconds() {
        // Any `.frac` on the seconds group is truncated (not rounded).
        assert_eq!(parse_ps_time("00:01.50"), 1 * NS);
        assert_eq!(parse_ps_time("00:00:09.999"), 9 * NS);
    }

    #[test]
    fn ps_time_garbage_groups_parse_as_zero() {
        // Unparsable groups contribute 0 rather than erroring.
        assert_eq!(parse_ps_time("xx:yy"), 0);
    }

    #[test]
    fn first_sample_has_zero_cpu_delta() {
        // Regression: `docker stats --no-stream` / the first stream sample computes CPU% from
        // (cpu_stats - precpu). With precpu seeded 0/0 the first delta was the pid's LIFETIME cpu over a
        // synthetic 100s window, so the reported % equalled the container's total CPU-seconds (>100%).
        // The first sample must seed precpu = its own totals so both deltas are 0 (⇒ 0%).
        let base = std::time::Instant::now();
        let pid = Some(std::process::id());
        let (v, total, system) = stats_sample("t", "id", pid, STATS_DEFAULT_LIMIT, 0, base, 0, 0);
        assert_eq!(
            v.precpu_stats.cpu_usage.total_usage, v.cpu_stats.cpu_usage.total_usage,
            "first sample precpu.total must equal cpu.total (zero cpuDelta)"
        );
        assert_eq!(
            v.precpu_stats.system_cpu_usage, v.cpu_stats.system_cpu_usage,
            "first sample precpu.system must equal cpu.system (zero systemDelta)"
        );
        // A LATER sample (idx>0) uses the threaded prior totals, so its precpu is the real prior reading.
        let (v2, ..) = stats_sample("t", "id", pid, STATS_DEFAULT_LIMIT, 1, base, total, system);
        assert_eq!(v2.precpu_stats.cpu_usage.total_usage, total);
        assert_eq!(v2.precpu_stats.system_cpu_usage, system);
    }
}
