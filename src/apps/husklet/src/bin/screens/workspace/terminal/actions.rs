//! One narrow, live-window exercise for the headless product gate.

use super::*;
use std::io::Write;

pub(super) struct LiveActions;

impl LiveActions {
    /// Drives the same controllers as a tab click, Paste shortcut, Close shortcut,
    /// and typed command. The receipt records requests; `HL_TERM_TEXT` remains the
    /// authority that the live guest received and executed both unique payloads.
    pub(super) fn schedule(window: &Rc<TermWin>) {
        let Some(receipt) = AppConfig::get().live_actions.clone() else {
            return;
        };
        if let Err(error) = std::fs::write(&receipt, "") {
            eprintln!("[husklet] live action receipt failed for {receipt}: {error}");
            return;
        }
        let names: Vec<_> = window.entries.borrow().iter().map(|entry| entry.name.clone()).collect();
        let Some(selected) = target(&names) else {
            Self::record(&receipt, "refused pages<3");
            return;
        };
        let plan = ActionPlan::new();
        // The last page is the split page produced by gui5. Select and later
        // close its preceding shell, so close must route focus back to that split.
        let window = window.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
            Page::new(&window, &selected).select_and_focus();
            Self::record(&receipt, &format!("selected {selected}"));

            let window = window.clone();
            let receipt = receipt.clone();
            let plan = plan.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                let Some(terminal) = window.focused.borrow().clone() else {
                    Self::record(&receipt, "failed paste:no-focus");
                    return;
                };
                terminal.clipboard().set_text(&plan.paste);
                Clipboard::paste(&window);
                Self::record(&receipt, "pasted live-paste-λ");

                let window = window.clone();
                let receipt = receipt.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
                    CurrentPage::close(&window);
                    Self::record(&receipt, "closed selected");

                    let window = window.clone();
                    let receipt = receipt.clone();
                    let plan = plan.clone();
                    glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                        let Some(terminal) = window.focused.borrow().clone() else {
                            Self::record(&receipt, "failed type:no-focus");
                            return;
                        };
                        terminal.feed_child(plan.after_close.as_bytes());
                        Self::record(&receipt, "typed live-after-close-終");
                    });
                });
            });
        });
    }

    fn record(path: &str, line: &str) {
        match std::fs::OpenOptions::new().create(true).append(true).open(path) {
            Ok(mut file) => {
                let _ = writeln!(file, "{line}");
            }
            Err(error) => eprintln!("[husklet] live action receipt failed for {path}: {error}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ActionPlan {
    marker: String,
    paste: String,
    after_close: String,
}

impl ActionPlan {
    fn new() -> Self {
        let elapsed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self::with_nonce(&format!("{}-{elapsed}", std::process::id()))
    }

    fn with_nonce(nonce: &str) -> Self {
        assert!(nonce.bytes().all(|byte| byte.is_ascii_digit() || byte == b'-'));
        let marker = format!("/tmp/.husklet-live-actions-{nonce}");
        let paste = format!("rm -f {marker}; printf '%s %s\\n' 'live-paste-λ' \"$$\" > {marker}\n");
        let after_close = format!(
            "value=$(cat {marker} 2>/dev/null); rm -f {marker}; set -- $value; test \"$1\" = 'live-paste-λ' && test \"$2\" != \"$$\" && printf '%s\\n' 'live-after-close-終 marker=live-paste-λ'\n"
        );
        Self {
            marker,
            paste,
            after_close,
        }
    }
}

fn target(names: &[String]) -> Option<String> {
    names
        .get(names.len().checked_sub(2)?)
        .filter(|_| names.len() >= 3)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_post_close_payload_can_only_report_after_consuming_the_paste_marker() {
        let plan = ActionPlan::with_nonce("17-42");
        assert_eq!(plan.marker, "/tmp/.husklet-live-actions-17-42");
        assert!(plan.paste.ends_with('\n'));
        assert!(plan.after_close.ends_with('\n'));
        assert!(plan.paste.contains("live-paste-λ"));
        let read = format!("value=$(cat {} 2>/dev/null)", plan.marker);
        let remove = format!("rm -f {}", plan.marker);
        assert!(plan.after_close.starts_with(&read));
        assert!(plan.after_close.contains(&remove));
        let check = "set -- $value; test \"$1\" = 'live-paste-λ'";
        assert!(plan.after_close.find(&remove).unwrap() < plan.after_close.find(check).unwrap());
        assert!(plan.after_close.contains("test \"$2\" != \"$$\""));
        assert!(plan.after_close.contains("live-after-close-終 marker=live-paste-λ"));
    }

    #[test]
    fn the_preceding_shell_is_closed_so_focus_must_cross_a_page_boundary() {
        let names = ["overview", "shell-1", "split-shell"].map(str::to_owned);
        assert_eq!(target(&names).as_deref(), Some("shell-1"));
        assert_eq!(target(&names[..2]), None);
    }

    #[test]
    fn the_live_product_window_schedules_the_action_sequence_once() {
        let source = include_str!("mod.rs");
        assert_eq!(source.matches("LiveActions::schedule(&tw);").count(), 1);
    }
}
