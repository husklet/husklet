//! The humanized `docker ps` Status column renderer (`human_status`).
use super::*;

/// Render a container's `docker ps` Status column the way docker does: "Up 3 minutes" while
/// running/paused (measured from `StartedAt`), "Exited (0) 5 minutes ago" otherwise (measured from
/// `FinishedAt`). Elapsed time is humanized coarsely (seconds/minutes/hours/days). Falls back to
/// `created` when the relevant timestamp was never set (legacy/edge state).
pub(crate) fn human_status(c: &Container) -> String {
    // Coarse elapsed-since humanizer for a base unix-secs timestamp.
    let humanize = |base: i64| -> String {
        let secs = (now_secs() - base).max(0);
        if secs < 60 {
            format!("{secs} seconds")
        } else if secs < 3600 {
            format!("{} minutes", secs / 60)
        } else if secs < 86400 {
            format!("{} hours", secs / 3600)
        } else {
            format!("{} days", secs / 86400)
        }
    };
    let started = if c.started_at > 0 {
        c.started_at
    } else {
        c.created
    };
    let finished = if c.finished_at > 0 {
        c.finished_at
    } else {
        c.created
    };
    if c.status == "restarting" {
        // Docker shows a container in its restart-backoff window as "Restarting (code) …" from the
        // last exit time.
        format!("Restarting ({}) {} ago", c.exit_code, humanize(finished))
    } else if c.status == "running" || c.status == "paused" {
        // "Up X" is measured from StartedAt, NOT creation time.
        format!("Up {}", humanize(started))
    } else if c.status == "created" {
        // A created-but-never-started container shows a bare "Created" (no elapsed time), matching docker.
        "Created".to_string()
    } else {
        // "Exited (code) X ago" is measured from FinishedAt.
        format!("Exited ({}) {} ago", c.exit_code, humanize(finished))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_status_up_measured_from_started_not_created() {
        // Regression: a container CREATED long ago but STARTED recently must read its recent uptime.
        let mut c = ctr();
        let now = now_secs();
        c.created = now - 100_000; // ~27h ago
        c.started_at = now - 90; // 90s ago
        assert_eq!(human_status(&c), "Up 1 minutes");
    }

    #[test]
    fn human_status_exited_measured_from_finished() {
        let mut c = ctr();
        let now = now_secs();
        c.status = "exited".into();
        c.exit_code = 0;
        c.created = now - 100_000;
        c.finished_at = now - 5; // 5s ago
        assert_eq!(human_status(&c), "Exited (0) 5 seconds ago");
    }

    #[test]
    fn human_status_falls_back_to_created_when_no_started_at() {
        // Legacy/edge: started_at unset (0) -> fall back to `created` so we still show elapsed.
        let mut c = ctr();
        let now = now_secs();
        c.created = now - 3661; // ~1h ago
        c.started_at = 0;
        assert_eq!(human_status(&c), "Up 1 hours");
    }

    #[test]
    fn human_status_created_is_bare() {
        let mut c = ctr();
        c.status = "created".into();
        // A never-started container is a bare "Created" with no elapsed time (time-independent).
        assert_eq!(human_status(&c), "Created");
    }

    #[test]
    fn human_status_prefix_by_state() {
        let mut c = ctr();
        c.created = now_secs(); // ~0 elapsed; assert only the state-dependent prefix.
        c.status = "running".into();
        assert!(human_status(&c).starts_with("Up "), "{}", human_status(&c));
        c.status = "exited".into();
        c.exit_code = 2;
        assert!(
            human_status(&c).starts_with("Exited (2) "),
            "{}",
            human_status(&c)
        );
        c.status = "restarting".into();
        c.exit_code = 1;
        assert!(
            human_status(&c).starts_with("Restarting (1) "),
            "{}",
            human_status(&c)
        );
    }
}
