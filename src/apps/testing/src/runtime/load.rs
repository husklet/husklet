//! Host contention recorded beside each result, so a contended run reads differently from a real timeout.

const UNMEASURED: &str = "unmeasured";

/// One-minute load average over the logical CPU count, as `load/cpus`.
pub(super) fn sample() -> String {
    let cpus = std::thread::available_parallelism().map_or(0, std::num::NonZeroUsize::get);
    average().map_or_else(|| UNMEASURED.to_owned(), |load| format!("{load:.2}/{cpus}"))
}

pub(super) fn unmeasured() -> String {
    UNMEASURED.to_owned()
}

#[cfg(target_os = "linux")]
fn average() -> Option<f64> {
    first_average(&std::fs::read_to_string("/proc/loadavg").ok()?)
}

#[cfg(target_os = "macos")]
fn average() -> Option<f64> {
    let output = std::process::Command::new("/usr/sbin/sysctl")
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
    use super::{sample, sanitize, unmeasured};

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
