use super::Form;
use crate::*;

impl Form {
    pub(crate) fn configuration(&self) -> std::io::Result<WorkspaceConfig> {
        let name = self.name.text().trim().to_string();
        let image = self.image.text().trim().to_string();
        if name.is_empty() || image.is_empty() {
            return Err(Self::invalid("Workspace name and image are required."));
        }
        let arch = if self.cpu_amd.get() { Arch::Amd64 } else { Arch::Arm64 };
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
        workspace.vpn = self.vpn()?;
        for (key, value) in self.env_rows.borrow().iter() {
            Self::push_environment(&mut workspace.env, &key.text(), &value.text())?;
        }
        for (host, container, readonly) in self.mount_rows.borrow().iter() {
            Self::push_mount(
                &mut workspace.mounts,
                &host.text(),
                &container.text(),
                readonly.is_active(),
            )?;
        }
        Ok(workspace)
    }

    fn environment_row(key: &str, value: &str) -> std::io::Result<Option<(String, String)>> {
        let key = key.trim();
        if key.is_empty() && value.is_empty() {
            return Ok(None);
        }
        if key.is_empty() {
            return Err(Self::invalid(
                "Environment variable name is required when a value is provided.",
            ));
        }
        if key.contains('=') || key.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(Self::invalid(
                "Environment variable names cannot contain '=' or control characters.",
            ));
        }
        Ok(Some((key.to_owned(), value.to_owned())))
    }

    fn push_environment(environment: &mut Vec<(String, String)>, key: &str, value: &str) -> std::io::Result<()> {
        let Some(variable) = Self::environment_row(key, value)? else {
            return Ok(());
        };
        if environment.iter().any(|(existing, _)| existing == &variable.0) {
            return Err(Self::invalid("Environment variable names must be unique."));
        }
        environment.push(variable);
        Ok(())
    }

    fn mount_row(host: &str, container: &str, read_only: bool) -> std::io::Result<Option<Mount>> {
        let host = host.trim();
        let container = container.trim();
        if host.is_empty() && container.is_empty() {
            return Ok(None);
        }
        if host.is_empty() || container.is_empty() {
            return Err(Self::invalid("Mount host and container paths are both required."));
        }
        if !std::path::Path::new(host).is_absolute() {
            return Err(Self::invalid("Mount host paths must be absolute."));
        }
        if !hl_container::normalized_mount_target(container) {
            return Err(Self::invalid("Mount container paths must be normalized and absolute."));
        }
        Ok(Some(Mount {
            host: host.to_owned(),
            container: container.to_owned(),
            ro: read_only,
        }))
    }

    fn push_mount(mounts: &mut Vec<Mount>, host: &str, container: &str, read_only: bool) -> std::io::Result<()> {
        let Some(mount) = Self::mount_row(host, container, read_only)? else {
            return Ok(());
        };
        if mounts.iter().any(|existing| existing.container == mount.container) {
            return Err(Self::invalid("Mount container paths must be unique."));
        }
        mounts.push(mount);
        Ok(())
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
                .ok_or_else(|| Self::invalid("Scrollback must be a positive number or “unlimited”.")),
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

    fn invalid(message: &str) -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, message)
    }
}

#[cfg(test)]
mod tests {
    use super::Form;

    #[test]
    fn partial_environment_rows_are_rejected_instead_of_discarded() {
        assert!(Form::environment_row("", "value").is_err());
        assert_eq!(Form::environment_row("", "").unwrap(), None);
        assert_eq!(
            Form::environment_row(" NAME ", "").unwrap(),
            Some(("NAME".into(), String::new()))
        );
    }

    #[test]
    fn environment_values_preserve_intentional_whitespace() {
        assert_eq!(
            Form::environment_row(" FLAGS ", "  -O2 -g  ").unwrap(),
            Some(("FLAGS".into(), "  -O2 -g  ".into()))
        );
    }

    #[test]
    fn unpersistable_environment_names_are_rejected() {
        for key in ["BAD=NAME", "BAD\nNAME", "BAD\tNAME", "BAD\x7fNAME"] {
            assert!(Form::environment_row(key, "value").is_err(), "accepted {key:?}");
        }
        assert_eq!(
            Form::environment_row("CARGO-FLAGS", "value").unwrap(),
            Some(("CARGO-FLAGS".into(), "value".into()))
        );
    }

    #[test]
    fn duplicate_environment_names_are_rejected_without_case_folding() {
        let mut environment = Vec::new();
        Form::push_environment(&mut environment, "PATH", "/first").unwrap();
        assert!(Form::push_environment(&mut environment, " PATH ", "/second").is_err());
        Form::push_environment(&mut environment, "Path", "/case-sensitive").unwrap();
        Form::push_environment(&mut environment, "EMPTY", "").unwrap();

        assert_eq!(
            environment,
            [
                ("PATH".into(), "/first".into()),
                ("Path".into(), "/case-sensitive".into()),
                ("EMPTY".into(), String::new()),
            ]
        );
    }

    #[test]
    fn partial_mount_rows_are_rejected_instead_of_discarded() {
        assert!(Form::mount_row("/host", "", false).is_err());
        assert!(Form::mount_row("", "/guest", false).is_err());
        assert_eq!(Form::mount_row("", "", false).unwrap(), None);

        let mount = Form::mount_row(" /host ", " /guest ", true).unwrap().unwrap();
        assert_eq!(mount.host, "/host");
        assert_eq!(mount.container, "/guest");
        assert!(mount.ro);
    }

    #[test]
    fn mounts_rejected_by_runtime_validation_are_rejected_before_save() {
        assert!(Form::mount_row("relative", "/guest", false).is_err());
        assert!(Form::mount_row("/host", "relative", false).is_err());
        assert!(Form::mount_row("/host", "/guest/../escape", false).is_err());
        assert!(Form::mount_row("/host", "/guest/./nested", false).is_err());
        assert!(Form::mount_row("/host", "/guest//nested", false).is_err());
        assert!(Form::mount_row("/host", "/guest/nested/", false).is_err());
        assert!(Form::mount_row("/host", "/guest/.config", false).is_ok());

        let mut mounts = Vec::new();
        Form::push_mount(&mut mounts, "/first", "/guest", false).unwrap();
        assert!(Form::push_mount(&mut mounts, "/second", " /guest ", true).is_err());
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].host, "/first");
    }
}
