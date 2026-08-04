use crate::*;

mod poll;
mod process;
mod resources;
mod settings;
mod summary;
mod table;

pub(crate) use process::*;
pub(crate) use resources::*;

use poll::{spawn_overview_poller, OverviewData};
use table::Table;

pub(crate) struct Overview<'a> {
    workspace: &'a WorkspaceConfig,
    page: Option<screens::workspace::Page>,
}

impl<'a> Overview<'a> {
    pub(crate) fn new(workspace: &'a WorkspaceConfig, page: Option<screens::workspace::Page>) -> Self {
        Self { workspace, page }
    }

    pub(crate) fn view(&self) -> gtk::Box {
        let ws = self.workspace;
        use screens::workspace::Page as WorkspacePage;

        // Live panes fed by a background poller over the workspace daemon's Unix socket.
        let data = std::sync::Arc::new(std::sync::Mutex::new(OverviewData::loading()));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        spawn_overview_poller(ws.name.clone(), self.shell_label(), data.clone(), stop.clone());
        let containers = Table::new(&["NAME", "IMAGE", "STATUS"]);
        let images = Table::new(&["REPOSITORY", "IMAGE ID", "SIZE"]);
        let volumes = Table::new(&["NAME", "DRIVER"]);
        let networks = Table::new(&["NAME", "DRIVER", "SCOPE"]);
        let (ppane, pbody) = live_proc_pane();
        let view = screens::workspace::View::new([
            (WorkspacePage::Overview, self.overview().upcast()),
            (WorkspacePage::Containers, containers.widget.clone().upcast()),
            (WorkspacePage::Images, images.widget.clone().upcast()),
            (WorkspacePage::Volumes, volumes.widget.clone().upcast()),
            (WorkspacePage::Networks, networks.widget.clone().upcast()),
            (WorkspacePage::Processes, ppane.upcast()),
            (WorkspacePage::Settings, self.settings().upcast()),
        ]);
        let weak_view = view.widget.downgrade();
        let workspace = ws.name.clone();
        let last = RefCell::new(None);
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            if weak_view.upgrade().is_none() {
                stop.store(true, std::sync::atomic::Ordering::Release);
                return glib::ControlFlow::Break;
            }
            let d = data.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
            if last.borrow().as_ref() == Some(&d) {
                return glib::ControlFlow::Continue;
            }
            containers.fill(&d.containers, d.resources_error.as_deref());
            images.fill(&d.images, d.resources_error.as_deref());
            volumes.fill(&d.volumes, d.resources_error.as_deref());
            networks.fill(&d.networks, d.resources_error.as_deref());
            fill_proc_table(&pbody, &workspace, &d.processes, d.processes_error.as_deref());
            *last.borrow_mut() = Some(d);
            glib::ControlFlow::Continue
        });

        // Debug: HL_TERM_OVERVIEW_PAGE selects a overview pane for screenshotting.
        if let Some(p) = AppConfig::get().overview_pane.as_deref() {
            view.select_name(p);
        } else if let Some(page) = self.page {
            view.select_name(page.title());
        }
        view.widget
    }

    fn shell_label(&self) -> String {
        self.workspace
            .shell
            .as_deref()
            .map(str::trim)
            .filter(|shell| !shell.is_empty())
            .and_then(|shell| shell.split_whitespace().next())
            .map(|shell| shell.rsplit('/').next().unwrap_or(shell).to_string())
            .unwrap_or_else(|| "bash".to_string())
    }
}
