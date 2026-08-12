use super::*;

pub(crate) struct WorkspaceProcesses<'a> {
    name: &'a str,
    shell: &'a str,
}

impl<'a> WorkspaceProcesses<'a> {
    pub(crate) fn new(name: &'a str, shell: &'a str) -> Self {
        Self { name, shell }
    }

    /// Reads launcher shells and their guest descendants from the host process table.
    pub(crate) fn read(&self) -> std::io::Result<Vec<Vec<String>>> {
        crate::host::process::Processes::snapshot()
            .map(|snapshot| filter_workspace_procs(&snapshot, self.name, self.shell))
    }
}

struct HostProcess {
    pid: String,
    ppid: String,
    etime: String,
    command: String,
}

impl HostProcess {
    fn launches(&self, workspace: &str) -> bool {
        let workspace = hl_ws::Workspace::storage_component(workspace);
        let marker = format!("--worker launch {workspace}");
        self.command.match_indices(&marker).any(|(index, _)| {
            let before = &self.command[..index];
            let after = &self.command[index + marker.len()..];
            before.chars().next_back().is_none_or(char::is_whitespace)
                && after.chars().next().is_none_or(char::is_whitespace)
        })
    }

    fn guest_command(&self) -> String {
        if let Some(index) = self.command.find(" --rootfs ") {
            let after = &self.command[index + " --rootfs ".len()..];
            if let Some(space) = after.find(' ') {
                let guest = after[space..].trim();
                if !guest.is_empty() {
                    return guest.chars().take(140).collect();
                }
            }
        }
        self.command
            .rsplit('/')
            .next()
            .unwrap_or(&self.command)
            .chars()
            .take(140)
            .collect()
    }
}

/// Adds every process whose parent is already kept, and reports whether that added anything.
///
/// `ps` output is in no particular order, so a child can precede its parent; repeating this until
/// it adds nothing is what closes the tree rather than one pass over the rows.
fn adopt_children(procs: &[HostProcess], keep: &mut std::collections::HashSet<String>) -> bool {
    let adopted: Vec<String> = procs
        .iter()
        .filter(|process| !keep.contains(&process.pid) && keep.contains(&process.ppid))
        .map(|process| process.pid.clone())
        .collect();
    keep.extend(adopted.iter().cloned());
    !adopted.is_empty()
}

/// Pure core of [`WorkspaceProcesses`] (unit-tested against a captured `ps` dump): given `ps -axo
/// pid=,ppid=,etime=,command=` output, return `[pid, ppid, name]` rows for the workspace's launcher
/// shells and everything under them. A launcher is a process whose command contains
/// `husklet --worker launch <name>`; descendants are found by
/// walking the ppid tree. Each shell is named by its `shell` binary + how long it has run (its `etime`) —
/// e.g. `bash · up 04:12` — which is meaningful and distinguishes sessions (the guest's own processes run
/// in-process and aren't individually visible host-side).
pub(crate) fn filter_workspace_procs(ps_text: &str, ws_name: &str, shell: &str) -> Vec<Vec<String>> {
    let procs: Vec<HostProcess> = ps_text
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 4 {
                return None;
            }
            Some(HostProcess {
                pid: parts[0].to_string(),
                ppid: parts[1].to_string(),
                etime: parts[2].to_string(),
                command: parts[3..].join(" "),
            })
        })
        .collect();

    // Every process whose argv is `… --worker launch <name>`. A guest fork inherits the launcher's
    // argv, so this set includes BOTH the real launcher shells and their in-guest forks.
    let launch_pids: std::collections::HashSet<String> = procs
        .iter()
        .filter(|process| process.launches(ws_name))
        .map(|p| p.pid.clone())
        .collect();
    // A launcher shell is a launch process whose PARENT is not itself a launcher (its parent is Husklet
    // or init); a launch process parented by another launcher is a guest fork, not a distinct shell.
    let mut keep: std::collections::HashSet<String> = launch_pids.clone();
    let roots: std::collections::HashSet<String> = procs
        .iter()
        .filter(|p| launch_pids.contains(&p.pid) && !launch_pids.contains(&p.ppid))
        .map(|p| p.pid.clone())
        .collect();
    // Transitively add descendants (guest forks are host children of a launcher).
    while adopt_children(&procs, &mut keep) {}

    procs
        .iter()
        .filter(|p| keep.contains(&p.pid))
        .map(|p| {
            let label = if roots.contains(&p.pid) {
                format!("{shell} · up {}", p.etime) // the shell binary + how long this session has run
            } else if p.command.contains(" --rootfs ") {
                p.guest_command()
            } else {
                "process".to_string() // a guest fork (retains the launcher's host argv)
            };
            vec![p.pid.clone(), p.ppid.clone(), label]
        })
        .collect()
}

/// The Processes pane: a header + a body that [`fill_proc_table`] repopulates with a NAME column and
/// per-row Stop (SIGTERM) / Force-kill (SIGKILL) buttons. These act on the host launcher process — i.e.
/// the terminal shell session — because the workspace's guest processes run inside the container
/// engine) and aren't individually visible in the host process table.
pub(crate) fn live_proc_pane() -> (gtk::ScrolledWindow, gtk::Box) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.add_css_class("dmain");
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    head.add_css_class("trow");
    head.add_css_class("thead");
    for (i, c) in ["PID", "PROCESS", "SIGNAL"].iter().enumerate() {
        let l = gtk::Label::new(Some(c));
        l.set_xalign(if i == 2 { 1.0 } else { 0.0 });
        l.set_hexpand(i == 1);
        l.set_width_chars(if i == 1 { 24 } else { 10 });
        l.add_css_class("tcell");
        head.append(&l);
    }
    outer.append(&head);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.set_hexpand(true);
    outer.append(&body);
    let sc = gtk::ScrolledWindow::builder()
        .child(&outer)
        .hexpand(true)
        .vexpand(true)
        .build();
    (sc, body)
}

pub(crate) fn fill_proc_table(body: &gtk::Box, workspace: &str, rows: &[Vec<String>], error: Option<&str>) {
    while let Some(c) = body.first_child() {
        body.remove(&c);
    }
    if let Some(e) = error {
        let l = gtk::Label::new(Some(e));
        l.add_css_class("dhint");
        l.set_margin_top(16);
        body.append(&l);
        return;
    }
    if rows.is_empty() {
        let l = gtk::Label::new(Some("— no shell sessions —"));
        l.add_css_class("dhint");
        l.set_margin_top(16);
        l.set_halign(gtk::Align::Start);
        body.append(&l);
        return;
    }
    for r in rows {
        ProcessTable::append_row(body, workspace, r);
    }
}

struct ProcessTable;

impl ProcessTable {
    fn append_row(body: &gtk::Box, workspace: &str, row_data: &[String]) {
        let Some(pid) = crate::host::process::ProcessId::parse(
            row_data.first().map(String::as_str).unwrap_or_default(),
            &hl_ws::Workspace::storage_component(workspace),
        ) else {
            return;
        };
        let name = row_data.get(2).cloned().unwrap_or_default();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.add_css_class("trow");
        row.add_css_class("tbody");
        let pl = gtk::Label::new(Some(&row_data[0]));
        pl.set_xalign(0.0);
        pl.set_width_chars(10);
        pl.add_css_class("tcell");
        let nl = gtk::Label::new(Some(&name));
        nl.set_xalign(0.0);
        nl.set_hexpand(true);
        nl.set_ellipsize(gtk::pango::EllipsizeMode::End);
        nl.add_css_class("tcell");
        row.append(&pl);
        row.append(&nl);
        // Per-row signal controls: graceful stop (SIGTERM) then force kill (SIGKILL).
        let stop = gtk::Button::from_icon_name("media-playback-stop-symbolic");
        stop.add_css_class("sigbtn");
        stop.set_tooltip_text(Some("Stop — send SIGTERM"));
        stop.set_valign(gtk::Align::Center);
        let stop_pid = pid.clone();
        stop.connect_clicked(move |_| {
            if let Err(error) = stop_pid.terminate() {
                hl_log::hl_warn!(hl_log::tag::RUNTIME, "workspace process stop ignored: {error}");
            }
        });
        let force = gtk::Button::from_icon_name("user-trash-symbolic");
        force.add_css_class("sigbtn");
        force.set_tooltip_text(Some("Force kill — send SIGKILL"));
        force.set_valign(gtk::Align::Center);
        force.connect_clicked(move |_| {
            if let Err(error) = pid.kill() {
                hl_log::hl_warn!(hl_log::tag::RUNTIME, "workspace process kill ignored: {error}");
            }
        });
        row.append(&stop);
        row.append(&force);
        body.append(&row);
    }
}
