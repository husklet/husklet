use crate::config::WorkspaceConfig;
use crate::paths;
use std::io;
use std::path::PathBuf;

pub(super) struct RestoreSummary {
    path: PathBuf,
}

impl RestoreSummary {
    pub(super) fn new(workspace: &WorkspaceConfig) -> Self {
        Self {
            path: workspace.storage_dir(&paths::hl_root()).join("state/restore.txt"),
        }
    }

    pub(super) fn publish(&self, failures: &[String]) -> io::Result<()> {
        if failures.is_empty() {
            return self.remove();
        }
        hl_fs::File::from(self.path.clone()).replace(Notice::render(failures))
    }

    pub(super) fn read(&self) -> io::Result<Option<String>> {
        match std::fs::read_to_string(&self.path) {
            Ok(summary) => Ok(Some(summary)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(super) fn clear(&self) -> io::Result<()> {
        self.remove()
    }

    fn remove(&self) -> io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// Prefix that attributes a restore line to Husklet itself.
///
/// Every line of the notice carries it, in the terminal and in the durable summary alike, so a
/// reader can never mistake Husklet's words for output their own program printed. The terminal
/// that renders a restored session filters lines carrying this prefix out of the scrollback it
/// persists, so a notice is shown once for the restore it describes and never replayed as history.
pub const NOTICE_PREFIX: &str = "husklet \u{2503} ";

/// Dim styling applied only where the notice is written to a terminal, never to the durable file.
const DIM: &str = "\u{1b}[2m";
const RESET: &str = "\u{1b}[0m";

/// Renders the restore outcome as words for the person who reopened the workspace.
///
/// The terminal gets one short line per program; the refusal's full engine reason goes to the log,
/// which is where a diagnostic of that length is actionable.
pub(super) struct Notice;

/// Shortens one refusal to a terminal-sized line and logs the full text it came from.
///
/// A refusal reads `execution <id>: <detail>`, and the detail repeats the same 32-hex id and then
/// explains an engine-internal boundary the reader cannot act on. Keep a short id as a reference,
/// keep the first clause of the reason, and drop the rest to the log.
fn summarize(failure: &str) -> String {
    hl_log::hl_info!(hl_log::tag::CONTAINER, "restore could not resume a program: {failure}");
    let Some((subject, detail)) = failure.split_once(": ") else {
        return failure.to_owned();
    };
    let subject = match subject.split_once(' ') {
        Some((noun, id)) if id.len() > 8 && id.chars().all(|c| c.is_ascii_hexdigit()) => {
            format!("{noun} {}", &id[..8])
        }
        _ => subject.to_owned(),
    };
    let clause = detail.split_once(": ").map_or(detail, |(head, _)| head);
    let clause = clause
        .strip_prefix("exec ")
        .and_then(|rest| rest.split_once(' '))
        .map_or(clause, |(_, words)| words);
    format!("{subject}: {clause}")
}

impl Notice {
    pub(super) fn render(failures: &[String]) -> String {
        let counted = if failures.len() == 1 {
            "1 program could not be resumed as a live process:".to_owned()
        } else {
            format!("{} programs could not be resumed as live processes:", failures.len())
        };
        let mut lines = vec![
            "Session restored. The output above is preserved history from your last session.".to_owned(),
            counted,
        ];
        lines.extend(
            failures
                .iter()
                .map(|failure| format!("  \u{2022} {}", summarize(failure))),
        );
        lines.push("Everything else came back. Run a command again when you need it live.".to_owned());
        lines
            .into_iter()
            .map(|line| format!("{NOTICE_PREFIX}{line}\n"))
            .collect()
    }

    /// The same notice, dimmed for a terminal that understands SGR styling.
    pub(super) fn for_terminal(summary: &str) -> String {
        summary.lines().map(|line| format!("{DIM}{line}{RESET}\n")).collect()
    }
}
