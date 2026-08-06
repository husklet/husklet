use super::{WorkspaceConfig, io, Arch, PathBuf, Mount, VpnConfig, CudaDevice, TerminalPreferences, Workspace};

#[derive(Default)]
pub(super) struct WorkspaceDocument {
    current: Option<WsBuilder>,
    items: Vec<WorkspaceConfig>,
}

impl WorkspaceDocument {
    pub(super) fn parse(text: &str) -> io::Result<Vec<WorkspaceConfig>> {
        let mut document = Self::default();
        for (index, line) in text.lines().map(str::trim).enumerate() {
            document.read(line).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("workspace configuration line {}: {error}", index + 1),
                )
            })?;
        }
        document.finish()?;
        Ok(document.items)
    }

    fn read(&mut self, line: &str) -> io::Result<()> {
        if line.is_empty() || line.starts_with('#') {
            return Ok(());
        }
        if line == "[workspace]" {
            self.finish()?;
            self.current = Some(WsBuilder::default());
            return Ok(());
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "expected `key = value` or `[workspace]`",
            ));
        };
        let builder = self.current.as_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "field appears before a `[workspace]` section",
            )
        })?;
        builder.set(key.trim(), value.trim())
    }

    fn finish(&mut self) -> io::Result<()> {
        if let Some(builder) = self.current.take() {
            let workspace = builder.build()?;
            self.push(workspace)?;
        }
        Ok(())
    }

    fn push(&mut self, workspace: WorkspaceConfig) -> io::Result<()> {
        if workspace.name.is_empty() {
            return Err(Value::new("workspace name", &workspace.name).invalid());
        }
        if workspace.image.is_empty() {
            return Err(Value::new("workspace image", &workspace.image).invalid());
        }
        if self.items.iter().any(|item| item.name == workspace.name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("duplicate workspace name {:?}", workspace.name),
            ));
        }
        self.items.push(workspace);
        Ok(())
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
    fn set(&mut self, k: &str, v: &str) -> io::Result<()> {
        match k {
            "name" => self.name = Some(v.to_string()),
            "image" => self.image = Some(v.to_string()),
            "arch" => self.arch = Some(Arch::parse(v).ok_or_else(|| Value::new("architecture", v).invalid())?),
            "storage" if !v.is_empty() => self.storage = Some(PathBuf::from(v)),
            "shell" if !v.is_empty() => self.shell = Some(v.to_string()),
            "cpus" => self.cpus = Some(Value::new("cpus", v).number()?),
            "memory" => self.memory_mb = Some(Value::new("memory", v).number()?),
            "docker_sock" => self.docker_sock = Some(Value::new("docker_sock", v).boolean()?),
            "gui" => self.gui = Some(Value::new("gui", v).boolean()?),
            "scrollback" => self.scrollback = Some(Value::new("scrollback", v).number()?),
            "vpn" if !v.is_empty() => {
                self.vpn = Some(VpnConfig::parse(v).ok_or_else(|| Value::new("vpn", v).invalid())?);
            }
            "cuda" if !v.is_empty() => {
                self.cuda = Some(CudaDevice::parse(v).ok_or_else(|| Value::new("cuda", v).invalid())?);
            }
            "terminal_font" if !v.is_empty() => self.terminal.font_family = Some(v.to_owned()),
            "terminal_size" => self.terminal.font_size = Some(Value::new("terminal_size", v).number()?),
            "terminal_foreground" if !v.is_empty() => self.terminal.foreground = Some(v.to_owned()),
            "terminal_background" if !v.is_empty() => self.terminal.background = Some(v.to_owned()),
            "terminal_cursor" if !v.is_empty() => self.terminal.cursor_shape = Some(v.to_owned()),
            "terminal_cursor_blink" => {
                self.terminal.cursor_blink = Some(Value::new("terminal_cursor_blink", v).boolean()?);
            }
            "env" => self.set_env(v)?,
            "mount" => self.set_mount(v)?,
            _ => return Err(Value::new("field", k).invalid()),
        }
        Ok(())
    }

    fn set_env(&mut self, value: &str) -> io::Result<()> {
        let Some((key, value)) = value.split_once('=') else {
            return Err(Value::new("environment", value).invalid());
        };
        if key.trim().is_empty() {
            return Err(Value::new("environment key", key).invalid());
        }
        self.env.push((key.trim().to_owned(), value.trim().to_owned()));
        Ok(())
    }

    fn set_mount(&mut self, value: &str) -> io::Result<()> {
        let mut fields = value.split(':');
        let (Some(host), Some(container)) = (fields.next(), fields.next()) else {
            return Err(Value::new("mount", value).invalid());
        };
        if host.is_empty() || container.is_empty() {
            return Err(Value::new("mount", value).invalid());
        }
        let mode = fields.next();
        if !matches!(mode, None | Some("ro" | "rw")) || fields.next().is_some() {
            return Err(Value::new("mount", value).invalid());
        }
        self.mounts.push(Mount {
            host: host.to_owned(),
            container: container.to_owned(),
            ro: mode == Some("ro"),
        });
        Ok(())
    }

    fn build(self) -> io::Result<WorkspaceConfig> {
        let name = self.name.ok_or_else(|| Self::missing("name"))?;
        let image = self.image.ok_or_else(|| Self::missing("image"))?;
        let arch = self.arch.ok_or_else(|| Self::missing("arch"))?;
        Ok(WorkspaceConfig {
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

    fn missing(field: &str) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, format!("workspace is missing {field}"))
    }
}

struct Value<'a> {
    field: &'a str,
    raw: &'a str,
}

impl<'a> Value<'a> {
    fn new(field: &'a str, raw: &'a str) -> Self {
        Self { field, raw }
    }

    fn boolean(&self) -> io::Result<bool> {
        match self.raw {
            "true" | "1" | "yes" | "on" => Ok(true),
            "false" | "0" | "no" | "off" => Ok(false),
            _ => Err(self.invalid()),
        }
    }

    fn number<T: std::str::FromStr>(&self) -> io::Result<T> {
        self.raw.parse().map_err(|_| self.invalid())
    }

    fn invalid(&self) -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid {} {:?}", self.field, self.raw),
        )
    }
}

pub(super) struct WorkspaceText {
    text: String,
    error: Option<io::Error>,
}

impl WorkspaceText {
    pub(super) fn new() -> Self {
        Self {
            text: "# hl workspaces\n".to_owned(),
            error: None,
        }
    }

    pub(super) fn section(&mut self) {
        self.text.push_str("\n[workspace]\n");
    }

    pub(super) fn field(&mut self, key: &str, value: &str) {
        if value.chars().any(|character| matches!(character, '\t' | '\n' | '\r')) {
            self.error.get_or_insert_with(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("workspace field {key:?} contains an unsupported control character"),
                )
            });
            return;
        }
        self.text.push_str(key);
        self.text.push_str(" = ");
        self.text.push_str(value);
        self.text.push('\n');
    }

    pub(super) fn into_string(self) -> io::Result<String> {
        self.error.map_or(Ok(self.text), Err)
    }
}
