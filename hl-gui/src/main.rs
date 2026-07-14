//! dd-app — a GTK4 desktop UI for the **dd** VM-less container runtime.
//!
//! Container-centric master/detail: a left sidebar lists containers + images (with the daemon
//! connection shown as a sidebar footer), and the content pane shows the selected item's detail —
//! a container's image/status/volumes/networks/ports/logs, or an image's run action. It is a thin
//! Docker-Engine-API client (`hl_client`) over the daemon's Unix socket, polled every couple seconds.
//!
//! Built only on macOS where the GTK stack is available (see the workspace `default-members` note).

#[cfg(target_os = "macos")]
mod mac;
mod daemon;
mod docker;
mod install;
mod snapshot;
mod ui;
mod update;

use hl_client::Client;
use gtk::prelude::GtkWindowExt; // for root.set_default_size on connect/disconnect
use relm4::prelude::*;
use snapshot::Snapshot;
use std::path::PathBuf;
use std::time::Duration;

/// Top-level navigation category (first sidebar).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Category {
    #[default]
    Home,
    Containers,
    Images,
    Networks,
    Volumes,
    Workspaces,
    System,
    Settings,
}

/// What the detail pane is currently showing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Selection {
    #[default]
    None,
    Container(String),
    Image(String),
    Network(String),
    Volume(String),
}

/// Messages from the UI (selection, button clicks, the refresh tick).
#[derive(Debug)]
enum Msg {
    ToggleDaemon,
    InstallCli,
    ConfirmReset,
    Reset,
    UpdateFound(update::Release),
    ApplyUpdate,
    SetContext(String),
    SetCategory(Category),
    Select(Selection),
    RunImage(String),
    StartContainer(String),
    StopContainer(String),
    RestartContainer(String),
    PauseContainer(String),
    UnpauseContainer(String),
    RemoveContainer(String),
    RemoveImage(String),
    RemoveNetwork(String),
    NewNetwork,
    CreateNetwork(String),
    RemoveVolume(String),
    NewVolume,
    CreateVolume(String),
    /// Add or replace a workspace (name, image, arch token) in `~/.dd/workspaces.conf`.
    CreateWorkspace(String, String, String),
    /// Remove a workspace by name.
    RemoveWorkspace(String),
}

/// Results delivered back to the UI thread from async work.
#[derive(Debug)]
enum Cmd {
    Snapshot(Box<Snapshot>),
    Logs(String, String),
    /// Shells detected in a container: (container id, shell basenames).
    Shells(String, Vec<String>),
}

/// One time-series sample for the Home sparklines (collected each 2s poll).
#[derive(Clone, Copy, Default)]
pub struct Sample {
    pub running: f64,
    pub containers: f64,
    pub images: f64,
    pub disk_gb: f64,
}

/// The application model.
struct AppModel {
    socket: PathBuf,
    snap: Snapshot,
    /// Rolling history (newest last, capped) backing the Home sparklines.
    history: std::collections::VecDeque<Sample>,
    /// Container ids currently being stopped+removed (shown with an orange "removing" status).
    removing: std::collections::HashSet<String>,
    /// Shells detected in the selected container: (id, shell basenames) — drives the ＋ menu.
    shells: Option<(String, Vec<String>)>,
    category: Category,
    selection: Selection,
    /// Logs for the currently selected container: `(container id, text)`.
    current_logs: Option<(String, String)>,
    /// Tracks connection transitions so we resize the window only when it flips.
    was_connected: bool,
    /// A newer release found on GitHub, if any (drives the "Update" button).
    update: Option<update::Release>,
    /// The daemon process we started (so we can stop it), if any.
    daemon_child: Option<std::process::Child>,
    /// Whether we've already offered to switch the docker context this session.
    context_prompted: bool,
}

impl Component for AppModel {
    type Init = PathBuf;
    type Input = Msg;
    type Output = ();
    type CommandOutput = Cmd;
    type Root = gtk::ApplicationWindow;
    type Widgets = ui::Widgets;

    fn init_root() -> Self::Root {
        gtk::ApplicationWindow::new(&relm4::main_application())
    }

    fn init(
        socket: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = AppModel {
            socket: socket.clone(),
            snap: Snapshot::default(),
            history: std::collections::VecDeque::new(),
            removing: std::collections::HashSet::new(),
            shells: None,
            // `DD_SHOT_VIEW` lets the screenshot harness open a specific panel for verification.
            category: match std::env::var("DD_SHOT_VIEW").as_deref() {
                Ok("containers") => Category::Containers,
                Ok("images") => Category::Images,
                Ok("networks") => Category::Networks,
                Ok("volumes") => Category::Volumes,
                Ok("workspaces") => Category::Workspaces,
                Ok("system") => Category::System,
                Ok("settings") => Category::Settings,
                _ => Category::Home,
            },
            selection: Selection::None,
            current_logs: None,
            was_connected: false,
            update: None,
            daemon_child: None,
            context_prompted: false,
        };
        let widgets = ui::build(&root, &sender);

        // Bundled starter images (hello-dd) are discovered straight from the app bundle by the daemon
        // (Resources/images), so an app update always serves the current set and nothing is copied into
        // ~/.dd that could go stale.

        // One-shot update check on startup (off the UI thread).
        {
            let sender = sender.clone();
            std::thread::spawn(move || {
                if let Some(rel) = update::check(env!("DD_VERSION")) {
                    sender.input(Msg::UpdateFound(rel));
                }
            });
        }

        // Background poll loop: fetch a snapshot every 2s until the component shuts down.
        sender.command(move |out, shutdown| {
            let socket = socket.clone();
            shutdown
                .register(async move {
                    let client = Client::new(&socket);
                    loop {
                        let snap = snapshot::fetch(&client).await;
                        if out.send(Cmd::Snapshot(Box::new(snap))).is_err() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                })
                .drop_on_shutdown()
        });

        // Headless verification: `DD_SHOT=/path.png` renders the live window to PNG once the daemon
        // snapshot has painted, then quits — so the UI can be checked without an interactive session.
        if let Ok(shot) = std::env::var("DD_SHOT") {
            root.set_default_size(1040, 680); // skip the compact onboarding size for the capture
            let win = root.clone();
            let delay = std::env::var("DD_SHOT_DELAY_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1800);
            gtk::glib::timeout_add_local_once(Duration::from_millis(delay), move || {
                match ui::screenshot(&win, &shot) {
                    Ok(()) => eprintln!("[dd-shot] wrote {shot}"),
                    Err(e) => eprintln!("[dd-shot] failed: {e}"),
                }
                std::process::exit(0);
            });
        }

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Msg, sender: ComponentSender<Self>, root: &Self::Root) {
        let socket = self.socket.clone();
        match msg {
            Msg::ToggleDaemon => {
                let delay = if self.snap.connected {
                    // Stop: kill the daemon we started, else bootout an installed LaunchAgent.
                    match self.daemon_child.take() {
                        Some(mut child) => {
                            let _ = child.kill();
                        }
                        None => daemon::stop_external_daemon(),
                    }
                    600
                } else {
                    self.daemon_child = daemon::spawn_daemon();
                    1300
                };
                sender.oneshot_command(async move {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    Cmd::Snapshot(Box::new(snapshot::fetch(&Client::new(&socket)).await))
                });
            }
            Msg::InstallCli => ui::show_cli_install(root),
            Msg::ConfirmReset => ui::confirm_reset(root, &sender),
            Msg::Reset => {
                self.selection = Selection::None;
                self.current_logs = None;
                sender.oneshot_command(async move {
                    let c = Client::new(&socket);
                    if let Ok(cs) = c.list_containers().await {
                        for ct in cs {
                            let _ = c.remove_container(&ct.id).await;
                        }
                    }
                    if let Ok(vs) = c.list_volumes().await {
                        for v in vs {
                            let _ = c.remove_volume(&v.name).await;
                        }
                    }
                    if let Ok(ns) = c.list_networks().await {
                        for n in ns {
                            if !matches!(n.name.as_str(), "bridge" | "host" | "none") {
                                let _ = c.remove_network(&n.id).await;
                            }
                        }
                    }
                    Cmd::Snapshot(Box::new(snapshot::fetch(&c).await))
                });
            }
            Msg::UpdateFound(rel) => self.update = Some(rel),
            Msg::ApplyUpdate => {
                if let Some(rel) = self.update.clone() {
                    std::thread::spawn(move || match update::install(&rel) {
                        Ok(()) => std::process::exit(0), // the freshly-installed copy is launching
                        Err(e) => eprintln!("update failed: {e}"),
                    });
                }
            }
            Msg::SetContext(name) => {
                sender.oneshot_command(async move {
                    docker::set_context(&name, &socket).await;
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    Cmd::Snapshot(Box::new(snapshot::fetch(&Client::new(&socket)).await))
                });
            }
            Msg::SetCategory(cat) => {
                if self.category != cat {
                    self.category = cat;
                    self.selection = Selection::None; // fresh pick from the new category
                    self.current_logs = None;
                }
            }
            Msg::Select(sel) => {
                self.selection = sel.clone();
                self.current_logs = None;
                if let Selection::Container(id) = sel {
                    snapshot::fetch_logs(&sender, self.socket.clone(), id.clone());
                    self.shells = None; // clear stale list until the probe returns
                    snapshot::fetch_shells(&sender, self.socket.clone(), id);
                }
            }
            Msg::RunImage(image) => {
                // Jump to Containers so the user sees the new one appear.
                self.category = Category::Containers;
                self.selection = Selection::None;
                self.act(sender, socket, move |c| async move {
                    let spec = hl_client::CreateContainer {
                        image,
                        ..Default::default()
                    };
                    if let Ok(id) = c.create_container(&spec).await {
                        let _ = c.start_container(&id).await;
                    }
                });
            }
            Msg::StartContainer(id) => self.act(sender, socket, move |c| async move {
                let _ = c.start_container(&id).await;
            }),
            Msg::StopContainer(id) => self.act(sender, socket, move |c| async move {
                let _ = c.stop_container(&id).await;
            }),
            Msg::RestartContainer(id) => self.act(sender, socket, move |c| async move {
                let _ = c.restart_container(&id).await;
            }),
            Msg::PauseContainer(id) => self.act(sender, socket, move |c| async move {
                let _ = c.pause_container(&id).await;
            }),
            Msg::UnpauseContainer(id) => self.act(sender, socket, move |c| async move {
                let _ = c.unpause_container(&id).await;
            }),
            Msg::RemoveContainer(id) => {
                // Mark it "removing" (orange) right away, then stop → remove gracefully. The amber clears
                // when the next snapshot no longer lists the container.
                self.removing.insert(id.clone());
                self.act(sender, socket, move |c| async move {
                    let _ = c.stop_container(&id).await;
                    let _ = c.remove_container(&id).await;
                });
            }
            Msg::RemoveImage(name) => {
                if self.selection == Selection::Image(name.clone()) {
                    self.selection = Selection::None;
                }
                self.act(sender, socket, move |c| async move {
                    let _ = c.remove_image(&name).await;
                });
            }
            Msg::RemoveNetwork(id) => {
                if self.selection == Selection::Network(id.clone()) {
                    self.selection = Selection::None;
                }
                self.act(sender, socket, move |c| async move {
                    let _ = c.remove_network(&id).await;
                });
            }
            Msg::NewNetwork => ui::prompt_name(
                root,
                "New network",
                "network name",
                &sender,
                Msg::CreateNetwork,
            ),
            Msg::NewVolume => ui::prompt_name(
                root,
                "New volume",
                "volume name",
                &sender,
                Msg::CreateVolume,
            ),
            Msg::CreateNetwork(name) => {
                self.selection = Selection::None;
                self.act(sender, socket, move |c| async move {
                    let _ = c.create_network(&name).await;
                });
            }
            Msg::RemoveVolume(name) => {
                if self.selection == Selection::Volume(name.clone()) {
                    self.selection = Selection::None;
                }
                self.act(sender, socket, move |c| async move {
                    let _ = c.remove_volume(&name).await;
                });
            }
            Msg::CreateVolume(name) => {
                self.selection = Selection::None;
                self.act(sender, socket, move |c| async move {
                    let _ = c.create_volume(&name).await;
                });
            }
            // Workspaces are a local config file (`~/.dd/workspaces.conf`), not a daemon resource — mutate
            // the store inline; the view reloads it on the next render (which Relm4 runs after update()).
            Msg::CreateWorkspace(name, image, arch) => {
                use hl_ws::{Arch, Workspace, WorkspaceStore};
                if let Some(arch) = Arch::parse(&arch) {
                    let path = ui::views::workspaces::workspaces_conf();
                    let mut store = WorkspaceStore::load(path);
                    let _ = store.upsert(Workspace::new(name, image, arch));
                }
            }
            Msg::RemoveWorkspace(name) => {
                use hl_ws::WorkspaceStore;
                let path = ui::views::workspaces::workspaces_conf();
                let mut store = WorkspaceStore::load(path);
                let _ = store.remove(&name);
            }
        }
    }

    fn update_cmd(&mut self, cmd: Cmd, sender: ComponentSender<Self>, root: &Self::Root) {
        match cmd {
            Cmd::Snapshot(s) => {
                self.snap = *s;
                // Drop "removing" markers for containers that are gone (removal finished).
                self.removing
                    .retain(|id| self.snap.containers.iter().any(|c| &c.id == id));
                // Append a sparkline sample (keep ~80 = a few minutes at the 2s poll).
                if self.snap.connected {
                    let running =
                        self.snap.containers.iter().filter(|c| c.running()).count() as f64;
                    let disk_gb = self
                        .snap
                        .df
                        .as_ref()
                        .map(|d| d.layers_size as f64 / 1.0e9)
                        .unwrap_or(0.0);
                    self.history.push_back(Sample {
                        running,
                        containers: self.snap.containers.len() as f64,
                        images: self.snap.images.len() as f64,
                        disk_gb,
                    });
                    while self.history.len() > 80 {
                        self.history.pop_front();
                    }
                }
                // Compact onboarding window when the daemon is off; expand when it comes up.
                if self.snap.connected != self.was_connected {
                    self.was_connected = self.snap.connected;
                    if self.snap.connected {
                        root.set_default_size(1040, 680);
                    } else {
                        root.set_default_size(660, 420);
                    }
                }
                // Offer once, on first data, to point the docker CLI at our daemon.
                if !self.context_prompted {
                    self.context_prompted = true;
                    if let Some(ctx) = self.snap.docker_context.clone() {
                        if ctx != "dd" {
                            ui::prompt_switch_context(root, &sender, &ctx);
                        }
                    }
                }
                // Auto-select the first item of the current category if nothing is selected.
                if self.selection == Selection::None {
                    match self.category {
                        Category::Home | Category::Settings | Category::Workspaces => {}
                        Category::Containers => {
                            if let Some(c) = self.snap.containers.first() {
                                self.selection = Selection::Container(c.id.clone());
                                snapshot::fetch_logs(&sender, self.socket.clone(), c.id.clone());
                            }
                        }
                        Category::Images => {
                            if let Some(i) = self.snap.images.first() {
                                self.selection = Selection::Image(i.name());
                            }
                        }
                        Category::Networks => {
                            if let Some(n) = self.snap.networks.first() {
                                self.selection = Selection::Network(n.id.clone());
                            }
                        }
                        Category::Volumes => {
                            if let Some(v) = self.snap.volumes.first() {
                                self.selection = Selection::Volume(v.name.clone());
                            }
                        }
                        Category::System => {}
                    }
                } else if let Selection::Container(id) = &self.selection {
                    // Keep the selected container's logs fresh.
                    snapshot::fetch_logs(&sender, self.socket.clone(), id.clone());
                }
            }
            Cmd::Logs(id, text) => {
                if let Selection::Container(sel) = &self.selection {
                    if sel == &id {
                        self.current_logs = Some((id, snapshot::last_lines(&text, 1000)));
                    }
                }
            }
            Cmd::Shells(id, shells) => {
                self.shells = Some((id, shells));
            }
        }
    }

    fn update_view(&self, widgets: &mut Self::Widgets, sender: ComponentSender<Self>) {
        ui::render(widgets, self, &sender);
    }
}

impl AppModel {
    /// Run a mutating action against the daemon, then refresh the snapshot — all off-thread.
    fn act<F, Fut>(&self, sender: ComponentSender<Self>, socket: PathBuf, f: F)
    where
        F: FnOnce(Client) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send,
    {
        sender.oneshot_command(async move {
            let client = Client::new(&socket);
            f(client.clone()).await;
            Cmd::Snapshot(Box::new(snapshot::fetch(&client).await))
        });
    }
}

fn main() {
    ui::setup_bundle_env();
    let socket = Client::default_socket();
    let app = RelmApp::new("com.dd.app");
    app.run::<AppModel>(socket);
}
