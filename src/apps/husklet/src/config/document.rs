use super::{io, Arch, Mount, PathBuf, TerminalPreferences, VpnConfig, Workspace, WorkspaceConfig};

#[derive(Default)]
pub(super) struct WorkspaceDocument {
    current: Option<WsBuilder>,
    items: Vec<WorkspaceConfig>,
}

impl WorkspaceDocument {
    pub(super) fn parse(text: &str) -> io::Result<Vec<WorkspaceConfig>> {
        let mut document = Self::default();
        for (index, line) in text.lines().enumerate() {
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
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return Ok(());
        }
        if trimmed == "[workspace]" {
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
        builder.set(key.trim(), value.strip_prefix(' ').unwrap_or(value))
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
    scrollback: ScrollbackValue,
    vpn: Option<VpnConfig>,
    terminal: TerminalPreferences,
}

#[derive(Default)]
enum ScrollbackValue {
    #[default]
    Missing,
    Unlimited,
    Lines(u64),
}

impl WsBuilder {
    fn set(&mut self, k: &str, v: &str) -> io::Result<()> {
        if k == "env" {
            return self.set_env(v);
        }
        let v = v.trim();
        match k {
            "name" => self.name = Some(v.to_string()),
            "image" => self.image = Some(v.to_string()),
            "arch" => self.arch = Some(Arch::parse(v).ok_or_else(|| Value::new("architecture", v).invalid())?),
            "storage" if !v.is_empty() => self.storage = Some(PathBuf::from(v)),
            "shell" if !v.is_empty() => self.shell = Some(v.to_string()),
            "cpus" => self.cpus = Some(Value::new("cpus", v).number()?),
            "memory" => self.memory_mb = Some(Value::new("memory", v).number()?),
            "docker_sock" => self.docker_sock = Some(Value::new("docker_sock", v).boolean()?),
            "scrollback" => {
                self.scrollback = match v.to_ascii_lowercase().as_str() {
                    "0" | "unlimited" => ScrollbackValue::Unlimited,
                    _ => {
                        let lines = Value::new("scrollback", v).number::<u64>()?;
                        if lines == 0 {
                            return Err(Value::new("scrollback", v).invalid());
                        }
                        ScrollbackValue::Lines(lines)
                    }
                };
            }
            "vpn" if !v.is_empty() => {
                self.vpn = Some(VpnConfig::parse(v).ok_or_else(|| Value::new("vpn", v).invalid())?);
            }
            "terminal_font" if !v.is_empty() => self.terminal.font_family = Some(v.to_owned()),
            "terminal_size" => self.terminal.font_size = Some(Value::new("terminal_size", v).number()?),
            "terminal_foreground" if !v.is_empty() => self.terminal.foreground = Some(v.to_owned()),
            "terminal_background" if !v.is_empty() => self.terminal.background = Some(v.to_owned()),
            "terminal_cursor" if !v.is_empty() => self.terminal.cursor_shape = Some(v.to_owned()),
            "terminal_cursor_blink" => {
                self.terminal.cursor_blink = Some(Value::new("terminal_cursor_blink", v).boolean()?);
            }
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
        self.env.push((key.trim().to_owned(), value.to_owned()));
        Ok(())
    }

    fn set_mount(&mut self, value: &str) -> io::Result<()> {
        if let Some(encoded) = value.strip_prefix("v2::") {
            return self.set_encoded_mount(value, encoded);
        }
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

    fn set_encoded_mount(&mut self, original: &str, encoded: &str) -> io::Result<()> {
        let mut fields = encoded.split(':');
        let (Some(host), Some(container), Some(mode)) = (fields.next(), fields.next(), fields.next()) else {
            return Err(Value::new("mount", original).invalid());
        };
        if fields.next().is_some() || !matches!(mode, "ro" | "rw") {
            return Err(Value::new("mount", original).invalid());
        }
        let host = decode_mount_path(host).ok_or_else(|| Value::new("mount", original).invalid())?;
        let container = decode_mount_path(container).ok_or_else(|| Value::new("mount", original).invalid())?;
        if host.is_empty() || container.is_empty() {
            return Err(Value::new("mount", original).invalid());
        }
        self.mounts.push(Mount {
            host,
            container,
            ro: mode == "ro",
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
            scrollback: match self.scrollback {
                ScrollbackValue::Missing => Some(super::DEFAULT_SCROLLBACK_LINES),
                ScrollbackValue::Unlimited => None,
                ScrollbackValue::Lines(lines) => Some(lines),
            },
            vpn: self.vpn,
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

    pub(super) fn mount(&mut self, mount: &Mount) {
        self.field(
            "mount",
            &format!(
                "v2::{}:{}:{}",
                encode_mount_path(&mount.host),
                encode_mount_path(&mount.container),
                if mount.ro { "ro" } else { "rw" }
            ),
        );
    }

    pub(super) fn into_string(self) -> io::Result<String> {
        self.error.map_or(Ok(self.text), Err)
    }
}

fn encode_mount_path(path: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(path.len() * 2);
    for byte in path.bytes() {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn decode_mount_path(encoded: &str) -> Option<String> {
    if !encoded.len().is_multiple_of(2) {
        return None;
    }
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        decoded.push(hex_value(pair[0])? << 4 | hex_value(pair[1])?);
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{WorkspaceConfig, WorkspaceStore};
    use hl_ws::Arch;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn environment_values_roundtrip_exactly_through_the_store() {
        let path = std::env::temp_dir().join(format!(
            "husklet-workspaces-env-{}-{}.conf",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut workspace = WorkspaceConfig::new("env-roundtrip", "ubuntu:24.04", Arch::Arm64);
        workspace.env = vec![
            ("FLAGS".into(), "  -O2 -g  ".into()),
            ("TOKEN".into(), "left=middle=right".into()),
            ("EMPTY".into(), String::new()),
            ("UNICODE".into(), " 中 🙂 ".into()),
        ];

        WorkspaceStore::load(&path).unwrap().upsert(workspace.clone()).unwrap();
        let loaded = WorkspaceStore::load(&path).unwrap();
        assert_eq!(loaded.get("env-roundtrip").unwrap().env, workspace.env);

        std::fs::remove_file(path).unwrap();
    }
}
