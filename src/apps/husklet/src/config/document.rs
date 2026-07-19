use super::*;

#[derive(Default)]
pub(super) struct WorkspaceDocument {
    current: Option<WsBuilder>,
    items: Vec<WorkspaceConfig>,
}

impl WorkspaceDocument {
    pub(super) fn parse(text: &str) -> Vec<WorkspaceConfig> {
        let mut document = Self::default();
        for line in text.lines().map(str::trim) {
            document.read(line);
        }
        document.finish();
        document.items
    }

    fn read(&mut self, line: &str) {
        if line.is_empty() || line.starts_with('#') {
            return;
        }
        if line == "[workspace]" {
            self.finish();
            self.current = Some(WsBuilder::default());
            return;
        }
        if self.current.is_none() && line.contains('\t') {
            self.legacy(line);
            return;
        }
        let Some((key, value)) = line.split_once('=') else {
            return;
        };
        if let Some(builder) = self.current.as_mut() {
            builder.set(key.trim(), value.trim());
        }
    }

    fn legacy(&mut self, line: &str) {
        let mut fields = line.splitn(3, '\t');
        let (Some(name), Some(arch), Some(image)) = (fields.next(), fields.next(), fields.next())
        else {
            return;
        };
        if let Some(arch) = Arch::parse(arch) {
            self.items.push(WorkspaceConfig::new(name, image, arch));
        }
    }

    fn finish(&mut self) {
        if let Some(workspace) = self.current.take().and_then(WsBuilder::build) {
            self.items.push(workspace);
        }
    }
}

#[derive(Default)]
struct WsBuilder {
    name: Option<String>,
    image: Option<String>,
    arch: Option<Arch>,
    storage: Option<PathBuf>,
    shell: Option<String>,
    cpus: Option<u32>,
    memory_mb: Option<u32>,
    env: Vec<(String, String)>,
    mounts: Vec<Mount>,
    docker_sock: Option<bool>,
    gui: Option<bool>,
    scrollback: Option<u64>,
    vpn: Option<VpnConfig>,
    cuda: Option<CudaDevice>,
    terminal: TerminalPreferences,
}

impl WsBuilder {
    fn set(&mut self, k: &str, v: &str) {
        match k {
            "name" => self.name = Some(v.to_string()),
            "image" => self.image = Some(v.to_string()),
            "arch" => self.arch = Arch::parse(v),
            "storage" if !v.is_empty() => self.storage = Some(PathBuf::from(v)),
            "shell" if !v.is_empty() => self.shell = Some(v.to_string()),
            "cpus" => self.cpus = v.parse().ok(),
            "memory" => self.memory_mb = v.parse().ok(),
            "docker_sock" => self.docker_sock = Some(matches!(v, "true" | "1" | "yes" | "on")),
            "gui" => self.gui = Some(matches!(v, "true" | "1" | "yes" | "on")),
            "scrollback" => self.scrollback = v.parse().ok(),
            "vpn" if !v.is_empty() => self.vpn = VpnConfig::parse(v),
            "cuda" if !v.is_empty() => self.cuda = CudaDevice::parse(v),
            "terminal_font" if !v.is_empty() => self.terminal.font_family = Some(v.to_owned()),
            "terminal_size" => self.terminal.font_size = v.parse().ok(),
            "terminal_foreground" if !v.is_empty() => self.terminal.foreground = Some(v.to_owned()),
            "terminal_background" if !v.is_empty() => self.terminal.background = Some(v.to_owned()),
            "terminal_cursor" if !v.is_empty() => self.terminal.cursor_shape = Some(v.to_owned()),
            "terminal_cursor_blink" => {
                self.terminal.cursor_blink = Some(matches!(v, "true" | "1" | "yes" | "on"))
            }
            "env" => self.set_env(v),
            "mount" => self.set_mount(v),
            _ => {}
        }
    }

    fn set_env(&mut self, value: &str) {
        let Some((key, value)) = value.split_once('=') else {
            return;
        };
        self.env
            .push((key.trim().to_owned(), value.trim().to_owned()));
    }

    fn set_mount(&mut self, value: &str) {
        let mut fields = value.split(':');
        let (Some(host), Some(container)) = (fields.next(), fields.next()) else {
            return;
        };
        if host.is_empty() || container.is_empty() {
            return;
        }
        self.mounts.push(Mount {
            host: host.to_owned(),
            container: container.to_owned(),
            ro: fields.next() == Some("ro"),
        });
    }

    fn build(self) -> Option<WorkspaceConfig> {
        let (name, image, arch) = (self.name?, self.image?, self.arch?);
        Some(WorkspaceConfig {
            ws: Workspace {
                name,
                image,
                arch,
                storage: self.storage,
                shell: self.shell,
                cpus: self.cpus,
                memory_mb: self.memory_mb,
                env: self.env,
                mounts: self.mounts,
            },
            docker_sock: self.docker_sock.unwrap_or(true),
            gui: self.gui.unwrap_or(false),
            scrollback: self.scrollback,
            vpn: self.vpn,
            cuda: self.cuda,
            terminal: self.terminal,
        })
    }
}

pub(super) struct WorkspaceText(String);

impl WorkspaceText {
    pub(super) fn new() -> Self {
        Self("# hl workspaces\n".to_owned())
    }

    pub(super) fn section(&mut self) {
        self.0.push_str("\n[workspace]\n");
    }

    pub(super) fn field(&mut self, key: &str, value: &str) {
        self.0.push_str(key);
        self.0.push_str(" = ");
        self.0.extend(
            value
                .chars()
                .filter(|character| !matches!(character, '\t' | '\n' | '\r')),
        );
        self.0.push('\n');
    }

    pub(super) fn into_string(self) -> String {
        self.0
    }
}
