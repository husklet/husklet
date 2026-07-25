use super::Form;
use crate::*;

impl Form {
    pub(crate) fn configuration(&self) -> std::io::Result<WorkspaceConfig> {
        let name = self.name.text().trim().to_string();
        let image = self.image.text().trim().to_string();
        if name.is_empty() || image.is_empty() {
            return Err(Self::invalid("Workspace name and image are required."));
        }
        let arch = if self.cpu_amd.get() {
            Arch::Amd64
        } else {
            Arch::Arm64
        };
        let mut workspace = WorkspaceConfig::new(&name, &image, arch);
        let shell = self.shell.text().trim().to_string();
        if !shell.is_empty() {
            workspace.shell = Some(shell);
        }
        let storage = self.storage.text().trim().to_string();
        if !storage.is_empty() {
            workspace.storage = Some(std::path::PathBuf::from(storage));
        }
        let cpus = self.cpus.value() as u32;
        if cpus > 0 {
            workspace.cpus = Some(cpus);
        }
        let memory = self.mem.value() as u32;
        if memory > 0 {
            workspace.memory_mb = Some(memory);
        }
        workspace.scrollback = self.scrollback()?;
        workspace.terminal = TerminalPreferences {
            font_family: Some(self.font.value()),
            font_size: Some(self.font_size.value().round() as u16),
            foreground: Some(self.foreground.value()),
            background: Some(self.background.value()),
            cursor_shape: Some(self.cursor.get().as_str().to_owned()),
            cursor_blink: Some(self.cursor_blink.is_active()),
        };
        workspace.docker_sock = self.features.docker.is_active();
        workspace.gui = self.features.graphical.is_active();
        workspace.vpn = self.vpn()?;
        workspace.cuda = self.cuda()?;
        for (key, value) in self.env_rows.borrow().iter() {
            let key = key.text().trim().to_string();
            if !key.is_empty() {
                workspace.env.push((key, value.text().trim().to_string()));
            }
        }
        for (host, container, readonly) in self.mount_rows.borrow().iter() {
            let host = host.text().trim().to_string();
            let container = container.text().trim().to_string();
            if !host.is_empty() && !container.is_empty() {
                workspace.mounts.push(Mount {
                    host,
                    container,
                    ro: readonly.is_active(),
                });
            }
        }
        Ok(workspace)
    }

    fn scrollback(&self) -> std::io::Result<Option<u64>> {
        let value = self.scrollback.text().trim().to_ascii_lowercase();
        match value.as_str() {
            "" | "0" | "unlimited" => Ok(None),
            _ => value
                .parse::<u64>()
                .ok()
                .filter(|value| *value > 0)
                .map(Some)
                .ok_or_else(|| {
                    Self::invalid("Scrollback must be a positive number or “unlimited”.")
                }),
        }
    }

    fn vpn(&self) -> std::io::Result<Option<VpnConfig>> {
        let value = self.features.vpn.text();
        if value.trim().is_empty() {
            return Ok(None);
        }
        VpnConfig::parse(value.trim())
            .map(Some)
            .ok_or_else(|| Self::invalid("VPN endpoint is invalid."))
    }

    fn cuda(&self) -> std::io::Result<Option<CudaDevice>> {
        if !self.features.cuda.is_active() {
            return Ok(None);
        }
        let spec = format!(
            "{}|{}|{}",
            self.features.cuda_name.text().trim(),
            self.features.cuda_capability.text().trim(),
            self.features.cuda_memory.text().trim()
        );
        CudaDevice::parse(&spec).map(Some).ok_or_else(|| {
            Self::invalid(
                "CUDA requires a name, numeric major.minor capability, and positive memory size.",
            )
        })
    }

    fn invalid(message: &str) -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, message)
    }
}
