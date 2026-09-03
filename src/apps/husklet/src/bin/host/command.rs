pub(crate) fn application_path() -> std::path::PathBuf {
    std::env::current_exe().unwrap_or_else(|_| "husklet".into())
}
