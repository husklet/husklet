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
            parent.set_default_size(913, 617);
            PaneView::new(&window, &terminal).split(gtk::Orientation::Horizontal);
            Tabs::new(&window).terminal();
            Self::prepare_initial_topology(&path_for_prompt, &app, &parent, &window);
            glib::ControlFlow::Break
        });
    }

    fn prepare_initial_topology(
        path: &str,
        app: &gtk::Application,
        parent: &gtk::ApplicationWindow,
        window: &Rc<TermWin>,
    ) {
        let path = path.to_owned();
        let app = app.clone();
        let parent = parent.clone();
        let window = window.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(20), move || {
            let Some(terminals) = Self::terminals(&window) else {
                return glib::ControlFlow::Continue;
            };
            for (slot, terminal) in terminals.iter().enumerate() {
                terminal.feed_child(Self::initial_command(slot).as_bytes());
            }
            let split_page = Page::of(&window, terminals[1].upcast_ref()).expect("split pane has no page");
            split_page.select();
            terminals[1].grab_focus();
            Self::record(
                &path,
                "initial_topology_typed tabs=2 panes=3 selected=split focused=1 geometry=913x617",
            );
            let ready = format!("{path}.initial-ready");

            let path_for_close = path.clone();
            let app = app.clone();
            let parent = parent.clone();
            let terminal_window = window.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(20), move || {
                if !std::path::Path::new(&ready).exists() || !Self::initial_state_visible(&terminal_window) {
                    return glib::ControlFlow::Continue;
                }
                Self::record(&path_for_close, "initial_state_ready");
                Self::record(&path_for_close, "close_requested");
                parent.close();
                Self::choose_continue(&path_for_close, &app, &parent, &terminal_window.ws, 1);
                glib::ControlFlow::Break
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
        let mut last_topology_failure = String::new();
        let mut topology_stage = 0;
        glib::timeout_add_local(std::time::Duration::from_millis(20), move || {
            let Some(terminals) = Self::terminals(&window) else {
                return glib::ControlFlow::Continue;
            };
            if topology_stage == 1 {
                if !Terminal::new(&terminals[2]).history().contains("history-slot-2") {
                    return glib::ControlFlow::Continue;
                }
                let Some(split_page) = Self::tab_for_slots(&window, &["0", "1"]) else {
                    return glib::ControlFlow::Continue;
                };
                Page::new(&window, &split_page).select();
                Panes::focus_when_mapped(&window, "1");
                topology_stage = 2;
                return glib::ControlFlow::Continue;
            }
            if let Err(failure) = Self::restored_topology(&parent, &window, &terminals) {
                if failure != last_topology_failure {
                    Self::record(&path, &format!("waiting_topology {failure}"));
                    last_topology_failure = failure;
                }
                return glib::ControlFlow::Continue;
            }
            if topology_stage == 0 {
                let Some(other_tab) = Self::tab_for_slots(&window, &["2"]) else {
                    return glib::ControlFlow::Continue;
                };
                Page::new(&window, &other_tab).select_and_focus();
                topology_stage = 1;
                Self::record(&path, &Self::event("history_probe_tab_selected", cycle));
                return glib::ControlFlow::Continue;
            }
            for (slot, terminal) in terminals.iter().enumerate() {
                terminal.feed_child(Self::reopen_command(slot, cycle).as_bytes());
            }
            Self::record(
                &path,
                &Self::event(
                    "topology_restored tabs=2 panes=3 selected=split focused=1 geometry=913x617",
                    cycle,
                ),
            );
            Self::await_reopen_receipts(&path, &app, &parent, &window, cycle);
            glib::ControlFlow::Break
        });
    }

    /// Accepts a restored cycle only after the terminal grids themselves show one acknowledgement per
    /// persisted slot. The slots are immutable layout identities, so this distinguishes three live restored
    /// panes from three writes accidentally routed through whichever pane happened to own keyboard focus.
    fn await_reopen_receipts(
        path: &str,
        app: &gtk::Application,
        parent: &gtk::ApplicationWindow,
        window: &Rc<TermWin>,
        cycle: usize,
    ) {
        let path = path.to_owned();
        let app = app.clone();
        let parent = parent.clone();
        let window = window.clone();
        let mut stage = 0;
        glib::timeout_add_local(std::time::Duration::from_millis(20), move || {
            if stage == 0 {
                let Some(other_tab) = Self::tab_for_slots(&window, &["2"]) else {
                    return glib::ControlFlow::Continue;
                };
                Page::new(&window, &other_tab).select_and_focus();
                stage = 1;
                return glib::ControlFlow::Continue;
            }
            let receipt = |slot: usize| {
                Window::pane(&window, &slot.to_string()).is_some_and(|terminal| {
                    Terminal::new(&terminal)
                        .history()
                        .contains(&format!("reopened-slot-{slot}-cycle-{cycle}"))
                })
            };
            if stage == 1 {
                if !receipt(2) {
                    return glib::ControlFlow::Continue;
                }
                let Some(split_tab) = Self::tab_for_slots(&window, &["0", "1"]) else {
                    return glib::ControlFlow::Continue;
                };
                Page::new(&window, &split_tab).select();
                Panes::focus_when_mapped(&window, "1");
                stage = 2;
                return glib::ControlFlow::Continue;
            }
            if !receipt(0) || !receipt(1) {
                return glib::ControlFlow::Continue;
            }
            Self::record(&path, &Self::event("reopen_receipts_visible", cycle));
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

    fn terminals(window: &Rc<TermWin>) -> Option<[vte4::Terminal; 3]> {
        Some([
            Window::pane(window, "0")?,
            Window::pane(window, "1")?,
            Window::pane(window, "2")?,
        ])
    }

    fn initial_command(slot: usize) -> String {
        const DIRECTORIES: [&str; 3] = ["/tmp", "/var", "/root"];
        format!(
            "HUSKLET_GUI_SLOT_{slot}=$(cat /proc/sys/kernel/random/uuid); export HUSKLET_GUI_SLOT_{slot}; cd {}; printf 'history-slot-{slot}\\n'; printf '%s|%s\\n' \"$HUSKLET_GUI_SLOT_{slot}\" \"$PWD\" > /tmp/husklet-gui-slot-{slot}-before; sleep 1000 & HUSKLET_GUI_BG_{slot}=$!; export HUSKLET_GUI_BG_{slot}; printf '%s\\n' \"$HUSKLET_GUI_BG_{slot}\" > /tmp/husklet-gui-background-{slot}-before\n",
            DIRECTORIES[slot]
        )
    }

    fn reopen_command(slot: usize, cycle: usize) -> String {
        format!(
            "printf '%s|%s\\n' \"$HUSKLET_GUI_SLOT_{slot}\" \"$PWD\" > /tmp/husklet-gui-slot-{slot}-after-{cycle}; kill -0 \"$HUSKLET_GUI_BG_{slot}\" && printf '%s\\n' \"$HUSKLET_GUI_BG_{slot}\" > /tmp/husklet-gui-background-{slot}-after-{cycle}; printf 'reopened-slot-{slot}-cycle-{cycle}\\n'\n"
        )
    }

    fn initial_state_visible(window: &Rc<TermWin>) -> bool {
        let Some(terminals) = Self::terminals(window) else {
            return false;
        };
        terminals.iter().enumerate().all(|(slot, terminal)| {
            Terminal::new(terminal)
                .history()
                .contains(&format!("history-slot-{slot}"))
        })
    }

    fn restored_topology(
        parent: &gtk::ApplicationWindow,
        window: &Rc<TermWin>,
        terminals: &[vte4::Terminal; 3],
    ) -> Result<(), String> {
        // The first entry is the non-closable workspace overview. Session persistence
        // deliberately skips it, so the journey's tab count must use the same domain.
        let tab_count = window
            .entries
            .borrow()
            .iter()
            .skip(1)
            .filter(|entry| entry.persisted)
            .count();
        if tab_count != 2 || parent.width() != 913 || parent.height() != 617 {
            return Err(format!(
                "tabs={tab_count} geometry={}x{}",
                parent.width(),
                parent.height()
            ));
        }
        let tabs = Window::tabs(window);
        let Some((split_page, _, split_slots)) = tabs
            .iter()
            .find(|(_, _, slots)| slots.iter().map(String::as_str).eq(["0", "1"]))
        else {
            return Err(format!("missing-split tabs={:?}", Self::tab_slots(&tabs)));
        };
        if !tabs
            .iter()
            .any(|(_, _, slots)| slots.iter().map(String::as_str).eq(["2"]))
            || split_slots.len() != 2
        {
            return Err(format!("missing-other-tab tabs={:?}", Self::tab_slots(&tabs)));
        }
        let focused_slot = window
            .focused
            .borrow()
            .as_ref()
            .and_then(|terminal| Slots::new(window).of(terminal));
        // The other tab has not been mapped yet, so VTE reports no rows for it
        // even though its replay buffer is populated. The caller selects that
        // tab and verifies slot 2 before returning here for the final check.
        let history = terminals[..2]
            .iter()
            .enumerate()
            .filter_map(|(slot, terminal)| {
                (!Terminal::new(terminal)
                    .history()
                    .contains(&format!("history-slot-{slot}")))
                .then_some(slot)
            })
            .collect::<Vec<_>>();
        if window.stack.visible_child_name().as_deref() != Some(split_page.as_str())
            || focused_slot.as_deref() != Some("1")
            || !history.is_empty()
        {
            let missing_history = history
                .iter()
                .map(|slot| (*slot, Terminal::new(&terminals[*slot]).tail(20).0))
                .collect::<Vec<_>>();
            return Err(format!(
                "selected={:?} expected={split_page} focused={focused_slot:?} missing-history={missing_history:?}",
                window.stack.visible_child_name()
            ));
        }
        Ok(())
    }

    fn tab_slots(tabs: &[(String, gtk::Widget, Vec<String>)]) -> Vec<Vec<String>> {
        tabs.iter().map(|(_, _, slots)| slots.clone()).collect()
    }

    fn tab_for_slots(window: &Rc<TermWin>, wanted: &[&str]) -> Option<String> {
        Window::tabs(window)
            .into_iter()
            .find(|(_, _, slots)| slots.iter().map(String::as_str).eq(wanted.iter().copied()))
            .map(|(name, _, _)| name)
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
