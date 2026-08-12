//! Host contention recorded beside each result, so a contended run reads differently from a real timeout.

const UNMEASURED: &str = "unmeasured";

/// Runnable-per-CPU ratio above which a timing is contended enough to be untrustworthy.
const SATURATED: f64 = 0.5;

/// One-minute load average over the logical CPU count, as `load/cpus`.
pub(crate) fn sample() -> String {
    average().map_or_else(|| UNMEASURED.to_owned(), |load| format!("{load:.2}/{}", cpus()))
}

/// Online CPUs rather than the affinity-restricted count, so a run pinned to one CPU still
/// reports the load of the box it competes with.
fn cpus() -> usize {
    #[cfg(target_os = "linux")]
    if let Ok(text) = std::fs::read_to_string("/sys/devices/system/cpu/online") {
        let counted = text
            .trim()
            .split(',')
            .filter_map(|span| {
                let (start, end) = span.split_once('-').unwrap_or((span, span));
                Some(end.parse::<usize>().ok()? - start.parse::<usize>().ok()? + 1)
            })
            .sum();
        if counted > 0 {
            return counted;
        }
    }
    std::thread::available_parallelism().map_or(0, std::num::NonZeroUsize::get)
}

pub(super) fn unmeasured() -> String {
    UNMEASURED.to_owned()
}

/// Whether a recorded `load/cpus` sample is contended enough that any timing taken beside it
/// must be reported as suspect rather than believed.
pub(crate) fn saturated(sample: &str) -> bool {
    let Some((load, cpus)) = sample.split_once('/') else {
        return false;
    };
    let (Ok(load), Ok(cpus)) = (load.trim().parse::<f64>(), cpus.trim().parse::<f64>()) else {
        return false;
    };
    cpus > 0.0 && load / cpus > SATURATED
}

#[cfg(target_os = "linux")]
fn average() -> Option<f64> {
    first_average(&std::fs::read_to_string("/proc/loadavg").ok()?)
}

#[cfg(target_os = "macos")]
fn average() -> Option<f64> {
    let output = crate::platform::HostProcess::standard("/usr/sbin/sysctl")
        .args(["-n", "vm.loadavg"])
        .output()
        .ok()?;
    first_average(std::str::from_utf8(&output.stdout).ok()?.trim_start_matches("{ "))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const fn average() -> Option<f64> {
    None
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn first_average(text: &str) -> Option<f64> {
    text.split_whitespace().next()?.parse().ok()
}

/// Keeps a sample from ever breaking the delimited row it is written into.
pub(super) fn sanitize(value: &str) -> String {
    let value = value.replace(['\t', '\n'], " ");
    if value.is_empty() { unmeasured() } else { value }
}

#[cfg(test)]
mod tests {
    use super::{sample, sanitize, saturated, unmeasured};

    #[test]
    fn a_contended_box_is_saturated_and_an_idle_one_is_not() {
        assert!(saturated("10.00/18"));
        assert!(!saturated("2.00/18"));
        assert!(!saturated(&unmeasured()));
        assert!(!saturated("garbage/18"));
        assert!(!saturated("1.00/0"));
    }

    #[test]
    fn a_sample_is_delimiter_free_and_never_empty() {
        let value = sample();
        assert!(!value.is_empty());
        assert!(!value.contains(['\t', '\n']), "{value}");
        assert_eq!(sanitize(&value), value);
    }

    #[test]
    fn unmeasured_replaces_empty_and_delimited_values() {
        assert_eq!(sanitize(""), unmeasured());
        assert_eq!(sanitize("1.00\t8"), "1.00 8");
    }
}
