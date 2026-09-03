//! Feature-gated production-executable checkpoint journey driver.

use super::*;
use std::io::Write as _;

pub(super) struct CheckpointJourney;

impl CheckpointJourney {
    pub(super) fn schedule(app: &gtk::Application, parent: &gtk::ApplicationWindow, window: &Rc<TermWin>) {
        let Some(path) = AppConfig::get().checkpoint_journey.clone() else {
            return;
        };
        if Self::contains(&path, "manager_reopen_clicked_cycle2") {
            Self::reopened(&path, app, parent, window, 2);
            return;
        }
        if Self::contains(&path, "manager_reopen_clicked") {
            Self::reopened(&path, app, parent, window, 1);
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
                b"HUSKLET_GUI_CONTINUITY=$(cat /proc/sys/kernel/random/uuid); export HUSKLET_GUI_CONTINUITY; printf '%s\\n' \"$HUSKLET_GUI_CONTINUITY\" > /tmp/husklet-gui-continuity-before; (while :; do printf x >> /tmp/husklet-gui-progress; sleep .05; done) & printf '%s\\n' \"$!\" > /tmp/husklet-gui-child\n",
            );
            Self::record(&path_for_prompt, "initial_command_typed");
            let path = path_for_prompt.clone();
            let app = app.clone();
            let parent = parent.clone();
            let terminal_window = window.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(800), move || {
                Self::record(&path, "close_requested");
                parent.close();
                Self::choose_continue(&path, &app, &parent, &terminal_window.ws, 1);
            });
            glib::ControlFlow::Break
        });
    }

    fn choose_continue(
        path: &str,
        app: &gtk::Application,
        parent: &gtk::ApplicationWindow,
        workspace: &WorkspaceConfig,
        cycle: usize,
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
            Self::record(&path, &Self::event("dialog_continue_clicked", cycle));
            button.emit_clicked();
            Self::await_offline(&path, &app, &workspace, cycle);
            glib::ControlFlow::Break
        });
    }

    fn await_offline(path: &str, app: &gtk::Application, workspace: &WorkspaceConfig, cycle: usize) {
        let path = path.to_owned();
        let app = app.clone();
        let workspace = workspace.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(20), move || {
            if std::os::unix::net::UnixStream::connect(hl::runtime::domain::Domain::new(&workspace).socket()).is_ok() {
                return glib::ControlFlow::Continue;
            }
            Self::record(&path, &Self::event("domain_offline", cycle));
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
            Self::record(&path, &Self::event("manager_reopen_clicked", cycle));
            launch.emit_clicked();
            glib::ControlFlow::Break
        });
    }

    fn reopened(
        path: &str,
        app: &gtk::Application,
        parent: &gtk::ApplicationWindow,
        window: &Rc<TermWin>,
        cycle: usize,
    ) {
        if Self::contains(path, "journey_complete") {
            return;
        }
        let path = path.to_owned();
        let app = app.clone();
        let parent = parent.clone();
        let window = window.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(20), move || {
            let Some(terminal) = Self::active_terminal(&window) else {
                return glib::ControlFlow::Continue;
            };
            terminal.feed_child(
                format!("printf '%s\\n' \"$HUSKLET_GUI_CONTINUITY\" > /tmp/husklet-gui-continuity-after-{cycle}\\n")
                    .as_bytes(),
            );
            Self::record(&path, &Self::event("reopen_command_typed", cycle));
            if cycle == 2 {
                Self::record(&path, "journey_complete");
                return glib::ControlFlow::Break;
            }
            let ready = format!("{path}.cycle1-ready");
            let path_for_close = path.clone();
            let app = app.clone();
            let parent = parent.clone();
            let terminal_window = window.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(20), move || {
                if !std::path::Path::new(&ready).exists() {
                    return glib::ControlFlow::Continue;
                }
                Self::record(&path_for_close, "close_requested_cycle2");
                parent.close();
                Self::choose_continue(&path_for_close, &app, &parent, &terminal_window.ws, 2);
                glib::ControlFlow::Break
            });
            glib::ControlFlow::Break
        });
    }

    fn event(base: &str, cycle: usize) -> String {
        if cycle == 1 {
            base.to_owned()
        } else {
            format!("{base}_cycle{cycle}")
        }
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
