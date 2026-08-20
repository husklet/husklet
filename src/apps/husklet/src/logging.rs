use hl_log::{Config, EnvironmentConfig, Level, Sink, Tags};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;

/// Apply Husklet's logging configuration at the composition boundary.
///
/// The base is every tag at `Error` and nothing above it. `Config::default()` is `Tags::NONE`, and the
/// tag mask gates `hl_error!` exactly as it gates `hl_trace!` -- so with the default an operation that
/// failed and named its reason produced no output at all. That is how a checkpoint refusal reached the
/// user as `CaptureRefused` with an empty log while the broker had recorded exactly why, and three lanes
/// in a row read the resulting silence as evidence about the engine. An error is never ordinary business
/// (see hl-log's level contract), so it is on by default and the environment only widens from here.
///
/// Passing that mask is only half of it. The signed application is launched by `launchd` with no
/// terminal attached, so the default stderr sink discards every line it is handed: a user who hit
/// "Could not close workspace" left no artifact behind, and the reason existed only in a dialog that
/// closed with the click that dismissed it. [`Journal`] gives those lines a file.
pub fn configure() {
    let base = Config {
        logging: Tags::ALL,
        level: Level::Error,
        ..Config::default()
    };
    let parsed = EnvironmentConfig::parse(base, std::env::vars());
    for warning in parsed.warnings() {
        eprintln!("husklet: {warning}");
    }
    parsed.apply();
    Journal::install();
}

/// The application's own log file, mirrored to stderr for terminal runs.
///
/// Appends rather than truncates, and opens once: several Husklet processes (the window and its
/// per-pane workers) share one path, and a line is written with a single `write_all` of an
/// already-formatted string, which `O_APPEND` keeps whole.
struct Journal(Mutex<std::fs::File>);

impl Journal {
    /// `~/.hl/husklet.log`.
    pub fn path() -> PathBuf {
        crate::paths::hl_root().join("husklet.log")
    }

    /// Best effort: a process that cannot open its log still runs, and still writes to stderr.
    fn install() {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            hl_log::Output::global().set(Box::new(Self(Mutex::new(file))));
        }
    }
}

impl Sink for Journal {
    fn write_line(&self, line: &str) {
        let stderr = std::io::stderr();
        let _ = stderr.lock().write_all(line.as_bytes());
        let mut file = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = file.write_all(line.as_bytes());
        let _ = file.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::Journal;

    #[test]
    fn the_journal_lives_beside_the_workspace_state_it_describes() {
        assert_eq!(Journal::path(), crate::paths::hl_root().join("husklet.log"));
    }

    /// The sink is what makes a `hl_error!` survive the click that dismissed the dialog. Write through
    /// the real `Sink` implementation rather than the global, which other tests in this binary share.
    #[test]
    fn a_written_line_reaches_the_file_and_is_appended_to() {
        use hl_log::Sink as _;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("husklet.log");
        std::fs::write(&path, "earlier line\n").unwrap();
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path).unwrap();
        let journal = Journal(std::sync::Mutex::new(file));

        journal.write_line("could not close workspace: guest fd 10 is a pipe\n");

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "earlier line\ncould not close workspace: guest fd 10 is a pipe\n"
        );
    }
}
