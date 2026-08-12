//! Host process inspection adapter.

pub(crate) fn process_sample(process_id: u64) -> std::io::Result<std::process::Output> {
    std::process::Command::new("ps")
        .args(["-o", "rss=,time=", "-p", &process_id.to_string()])
        .output()
}
