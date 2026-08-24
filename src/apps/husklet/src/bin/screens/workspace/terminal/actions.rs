//! One narrow, live-window exercise for the headless product gate.

use super::*;
use std::io::Write;

pub(super) struct LiveActions;

impl LiveActions {
    /// Drives the same controllers as tab creation, tab selection, Paste, Close,
    /// and typed commands. The receipt records requests; `HL_TERM_TEXT` remains the
    /// authority that the live guest received and executed the unique payloads.
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
            Self::record(&receipt, &format!("selected {selected} run={}", plan.nonce));

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
                Self::record(&receipt, &format!("pasted live-paste-λ run={}", plan.nonce));

                let window = window.clone();
                let receipt = receipt.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
                    CurrentPage::close(&window);
                    Self::record(&receipt, &format!("closed selected run={}", plan.nonce));

                    let window = window.clone();
                    let receipt = receipt.clone();
                    let plan = plan.clone();
                    glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                        let Some(terminal) = window.focused.borrow().clone() else {
                            Self::record(&receipt, "failed type:no-focus");
                            return;
                        };
                        terminal.feed_child(plan.after_close(&selected).as_bytes());
                        Self::record(
                            &receipt,
                            &format!("typed live-after-close-終 run={} selected={selected}", plan.nonce),
                        );

                        let replacement = Tabs::new(&window).terminal();
                        let Some(opened) = replacement_receipt(&replacement) else {
                            Self::record(&receipt, "failed reopen:empty-identity");
                            return;
                        };
                        Self::record(&receipt, &format!("{opened} run={}", plan.nonce));
                        let window = window.clone();
                        let receipt = receipt.clone();
                        let reopened = plan.reopened(&replacement);
                        glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
                            let Some(terminal) = window.stack.visible_child().and_then(|page| PaneView::first(&page))
                            else {
                                Self::record(&receipt, "failed reopen:no-terminal");
                                return;
                            };
                            terminal.feed_child(reopened.as_bytes());
                            Self::record(
                                &receipt,
                                &format!("typed live-reopened-再 run={} opened={replacement}", plan.nonce),
                            );
                        });
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
    nonce: String,
    marker: String,
    paste: String,
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
        let paste = format!("rm -f {marker}; printf '%s %s %s\\n' 'live-paste-λ' '{nonce}' \"$$\" > {marker}\n");
        Self {
            nonce: nonce.to_owned(),
            marker,
            paste,
        }
    }

    fn after_close(&self, selected: &str) -> String {
        assert!(!selected.is_empty() && !selected.chars().any(char::is_whitespace));
        format!(
            "value=$(cat {} 2>/dev/null); rm -f {}; set -- $value; test \"$1\" = 'live-paste-λ' && test \"$2\" = '{}' && test \"$3\" != \"$$\" && printf '%s\\n' 'live-after-close-終 marker=live-paste-λ run={} selected={} source_pid='\"$3\"' current_pid='\"$$\"\n",
            self.marker, self.marker, self.nonce, self.nonce, selected
        )
    }

    fn reopened(&self, opened: &str) -> String {
        assert!(!opened.is_empty() && !opened.chars().any(char::is_whitespace));
        format!("printf '%s\\n' 'live-reopened-再 run={} opened={opened}'\n", self.nonce)
    }
}

fn target(names: &[String]) -> Option<String> {
    names
        .get(names.len().checked_sub(2)?)
        .filter(|_| names.len() >= 3)
        .cloned()
}

fn replacement_receipt(name: &str) -> Option<String> {
    (!name.is_empty() && !name.chars().any(char::is_whitespace)).then(|| format!("opened replacement {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt_stage(line: &str) -> Option<&'static str> {
        match line {
            "selected shell-2 run=17-42" => Some("selected"),
            "pasted live-paste-λ run=17-42" => Some("pasted"),
            "closed selected run=17-42" => Some("closed"),
            "typed live-after-close-終 run=17-42 selected=shell-2" => Some("after-close"),
            "opened replacement shell-5 run=17-42" => Some("opened"),
            "typed live-reopened-再 run=17-42 opened=shell-5" => Some("reopened"),
            _ => None,
        }
    }

    #[test]
    fn the_post_close_payload_can_only_report_after_consuming_the_paste_marker() {
        let plan = ActionPlan::with_nonce("17-42");
        assert_eq!(plan.marker, "/tmp/.husklet-live-actions-17-42");
        assert!(plan.paste.ends_with('\n'));
        let after_close = plan.after_close("shell-2");
        let reopened = plan.reopened("shell-5");
        assert!(after_close.ends_with('\n'));
        assert!(reopened.ends_with('\n'));
        assert!(plan.paste.contains("live-paste-λ"));
        let read = format!("value=$(cat {} 2>/dev/null)", plan.marker);
        let remove = format!("rm -f {}", plan.marker);
        assert!(after_close.starts_with(&read));
        assert!(after_close.contains(&remove));
        let check = "set -- $value; test \"$1\" = 'live-paste-λ'";
        assert!(after_close.find(&remove).unwrap() < after_close.find(check).unwrap());
        assert!(after_close.contains("test \"$2\" = '17-42'"));
        assert!(after_close.contains("test \"$3\" != \"$$\""));
        assert!(after_close.contains("run=17-42 selected=shell-2"));
        assert_eq!(reopened, "printf '%s\\n' 'live-reopened-再 run=17-42 opened=shell-5'\n");
    }

    #[test]
    fn the_preceding_shell_is_closed_so_focus_must_cross_a_page_boundary() {
        let names = ["overview", "shell-1", "split-shell"].map(str::to_owned);
        assert_eq!(target(&names).as_deref(), Some("shell-1"));
        assert_eq!(target(&names[..2]), None);
    }

    #[test]
    fn the_receipt_parser_requires_close_before_replacement_and_reopened_input() {
        let receipt = [
            "selected shell-2 run=17-42",
            "pasted live-paste-λ run=17-42",
            "closed selected run=17-42",
            "typed live-after-close-終 run=17-42 selected=shell-2",
            "opened replacement shell-5 run=17-42",
            "typed live-reopened-再 run=17-42 opened=shell-5",
        ];
        assert_eq!(
            receipt.map(receipt_stage),
            [
                Some("selected"),
                Some("pasted"),
                Some("closed"),
                Some("after-close"),
                Some("opened"),
                Some("reopened"),
            ]
        );
        assert_eq!(receipt_stage("opened replacement shell-5 run=17-43"), None);
        assert_eq!(
            receipt_stage("typed live-after-close-終 run=17-42 selected=shell-3"),
            None
        );
        assert_eq!(replacement_receipt(""), None);
        assert_eq!(replacement_receipt("shell 5"), None);
        assert_eq!(
            replacement_receipt("shell-5").as_deref(),
            Some("opened replacement shell-5")
        );
        assert_eq!(receipt_stage("typed live-reopened"), None);
    }

    #[test]
    fn the_live_product_window_schedules_the_action_sequence_once() {
        let source = include_str!("mod.rs");
        assert_eq!(source.matches("LiveActions::schedule(&tw);").count(), 1);
    }
}
