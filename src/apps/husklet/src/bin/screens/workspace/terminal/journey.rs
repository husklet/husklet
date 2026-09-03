//! Feature-gated production-executable checkpoint journey driver.

use super::*;
use std::io::Write as _;

pub(super) struct CheckpointJourney;

impl CheckpointJourney {
    pub(super) fn schedule(app: &gtk::Application, parent: &gtk::ApplicationWindow, window: &Rc<TermWin>) {
        let Some(path) = AppConfig::get().checkpoint_journey.clone() else {
            return;
        };
        if Self::contains(&path, "manager_reopen_clicked") {
            Self::reopened(&path, window);
            return;
        }
        if std::path::Path::new(&path).exists() {
            return;
        }
        Self::record(&path, "opened_initial");
        let path_for_prompt = path.clone();
        let app = app.clone();
        let parent = parent.clone();
        let window = window.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(20), move || {
            let Some(terminal) = Self::active_terminal(&window) else {
                return glib::ControlFlow::Continue;
            };
            terminal.feed_child(
                b"if test -e /tmp/husklet-gui-guard; then echo fresh > /tmp/husklet-gui-fresh; else touch /tmp/husklet-gui-guard; printf '%s\\n' \"$$\" > /tmp/husklet-gui-shell-before; (while :; do printf x >> /tmp/husklet-gui-progress; sleep .05; done) & printf '%s\\n' \"$!\" > /tmp/husklet-gui-child; fi\n",
            );
            Self::record(&path_for_prompt, "initial_command_typed");
            let path = path_for_prompt.clone();
            let app = app.clone();
            let parent = parent.clone();
            let terminal_window = window.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(800), move || {
                Self::record(&path, "close_requested");
                parent.close();
                Self::choose_continue(&path, &app, &parent, &terminal_window.ws);
            });
            glib::ControlFlow::Break
        });
    }

    fn choose_continue(
        path: &str,
        app: &gtk::Application,
        parent: &gtk::ApplicationWindow,
        workspace: &WorkspaceConfig,
    ) {
        let path = path.to_owned();
        let app = app.clone();
        let parent = parent.clone();
        let parent_window: gtk::Window = parent.clone().upcast();
        let workspace = workspace.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(20), move || {
            let button = gtk::Window::toplevels()
                .iter::<gtk::Widget>()
                .filter_map(Result::ok)
                .filter_map(|candidate| candidate.downcast::<gtk::Window>().ok())
                .filter(|candidate| candidate != &parent_window)
                .find_map(|candidate| {
                    candidate
                        .child()
                        .and_then(|child| find_button(&child, "Continue later", None))
                });
            let Some(button) = button else {
                return glib::ControlFlow::Continue;
            };
            Self::record(&path, "dialog_continue_clicked");
            button.emit_clicked();
            Self::await_offline(&path, &app, &workspace);
            glib::ControlFlow::Break
        });
    }

    fn await_offline(path: &str, app: &gtk::Application, workspace: &WorkspaceConfig) {
        let path = path.to_owned();
        let app = app.clone();
        let workspace = workspace.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(20), move || {
            if std::os::unix::net::UnixStream::connect(hl::runtime::domain::Domain::new(&workspace).socket()).is_ok() {
                return glib::ControlFlow::Continue;
            }
            Self::record(&path, "domain_offline");
            let launch = app
                .windows()
                .into_iter()
                .find(|window| window.widget_name() == MANAGER_WINDOW_NAME)
                .and_then(|window| window.child())
                .and_then(|child| find_button(&child, "", Some("Launch workspace")));
            let Some(launch) = launch else {
                Self::record(&path, "failed_manager_launch_button_absent");
                return glib::ControlFlow::Break;
            };
            Self::record(&path, "manager_reopen_clicked");
            launch.emit_clicked();
            glib::ControlFlow::Break
        });
    }

    fn reopened(path: &str, window: &Rc<TermWin>) {
        if Self::contains(path, "journey_complete") {
            return;
        }
        let path = path.to_owned();
        let window = window.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(20), move || {
            let Some(terminal) = Self::active_terminal(&window) else {
                return glib::ControlFlow::Continue;
            };
            terminal.feed_child(b"printf '%s\\n' \"$$\" > /tmp/husklet-gui-shell-after\n");
            Self::record(&path, "reopen_command_typed");
            let complete = path.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
                Self::record(&complete, "journey_complete");
            });
            glib::ControlFlow::Break
        });
    }

    fn contains(path: &str, event: &str) -> bool {
        std::fs::read_to_string(path).is_ok_and(|text| text.lines().any(|line| line.ends_with(event)))
    }

    fn active_terminal(window: &TermWin) -> Option<vte4::Terminal> {
        window
            .focused
            .borrow()
            .clone()
            .or_else(|| window.stack.visible_child().and_then(|page| PaneView::first(&page)))
    }

    fn record(path: &str, event: &str) {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "time_ms={millis} {event}");
            let _ = file.sync_all();
        }
    }
}

fn find_button(widget: &gtk::Widget, label: &str, tooltip: Option<&str>) -> Option<gtk::Button> {
    if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        let label_matches = label.is_empty() || button.label().as_deref() == Some(label);
        if label_matches && tooltip.is_none_or(|wanted| button.tooltip_text().as_deref() == Some(wanted)) {
            return Some(button.clone());
        }
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(button) = find_button(&current, label, tooltip) {
            return Some(button);
        }
        child = current.next_sibling();
    }
    None
}
