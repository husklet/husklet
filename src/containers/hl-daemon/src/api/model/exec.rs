#[cfg(feature = "runtime")]
use super::{CompatibilityFields, EnvVars};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ExecOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ExecCatalogue {
    pub executions: Vec<ExecInspect>,
    pub truncated: bool,
}

#[cfg(feature = "runtime")]
#[hl_design::classify(domain = "docker")]
pub(crate) fn console_size(value: Option<[u64; 2]>) -> Result<Option<hl_container::Size>, String> {
    let Some([height, width]) = value else {
        return Ok(None);
    };
    if height == 0 && width == 0 {
        return Ok(None);
    }
    let rows = u16::try_from(height).map_err(|_| "terminal height exceeds 65535".to_owned())?;
    let columns = u16::try_from(width).map_err(|_| "terminal width exceeds 65535".to_owned())?;
    hl_container::Size::new(rows, columns)
        .map(Some)
        .map_err(|error| error.to_string())
}

/// Process streams selected when Docker creates and attaches in one operation.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct Attachment {
    #[serde(default, rename = "AttachStdin")]
    pub stdin: bool,
    #[serde(default, rename = "AttachStdout")]
    pub stdout: bool,
    #[serde(default, rename = "AttachStderr")]
    pub stderr: bool,
}

/// Docker exec-create request.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ExecConfig {
    #[serde(flatten)]
    pub attach: Attachment,
    #[serde(default, rename = "DetachKeys")]
    pub detach_keys: String,
    #[serde(default, rename = "Tty")]
    pub tty: bool,
    #[serde(default, rename = "Env", skip_serializing_if = "Option::is_none")]
    pub env: Option<Vec<String>>,
    #[serde(default, rename = "Cmd")]
    pub command: Vec<String>,
    #[serde(default, rename = "Privileged")]
    pub privileged: bool,
    #[serde(default, rename = "User")]
    pub user: String,
    #[serde(default, rename = "WorkingDir")]
    pub working_dir: String,
    #[serde(
        default,
        rename = "HuskletLifetime",
        skip_serializing_if = "ExecLifetime::is_default"
    )]
    pub lifetime: ExecLifetime,
    #[serde(default, rename = "HuskletNetwork", skip_serializing_if = "ExecNetwork::is_default")]
    pub network: ExecNetwork,
    #[serde(default, rename = "HuskletNative", skip_serializing_if = "is_false")]
    pub native: bool,
    #[serde(flatten, default)]
    pub unsupported: std::collections::BTreeMap<String, serde_json::Value>,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecLifetime {
    #[default]
    Persisted,
    Live,
    Ephemeral,
}

impl ExecLifetime {
    #[allow(clippy::trivially_copy_pass_by_ref)]
    fn is_default(value: &Self) -> bool {
        *value == Self::Persisted
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecNetwork {
    #[default]
    Container,
    Isolated,
}

impl ExecNetwork {
    #[allow(clippy::trivially_copy_pass_by_ref)]
    fn is_default(value: &Self) -> bool {
        *value == Self::Container
    }
}

#[cfg(feature = "runtime")]
impl ExecConfig {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if let Some(name) = CompatibilityFields::from(&self.unsupported).first_meaningful() {
            return Err(format!("unsupported exec create field {name}"));
        }
        if self.privileged {
            return Err("exec Privileged is not implemented".into());
        }
        if self.command.is_empty() {
            return Err("No exec command specified".into());
        }
        Ok(())
    }

    pub(crate) fn process(&self, parent: &hl_container::Process) -> Result<hl_container::Process, String> {
        self.validate()?;
        let (program, arguments) = self
            .command
            .split_first()
            .ok_or_else(|| "No exec command specified".to_owned())?;
        let mut process = parent.clone();
        process.program.clone_from(program);
        process.args = arguments.to_vec();
        process.console = hl_container::Console {
            stdin: self.attach.stdin,
            terminal: self.tty.then(hl_container::Size::default),
        };
        process.env.extend(
            EnvVars::parse(self.env.clone().unwrap_or_default())
                .map_err(|error| error.to_string())?
                .into_inner(),
        );
        if !self.working_dir.is_empty() {
            process.working_dir = self.working_dir.clone().into();
        }
        // `User` is resolved against the container's /etc/passwd and /etc/group by the container
        // domain, which owns the root filesystem; the process inherits the parent identity until then.
        Ok(process)
    }
}

/// Docker exec-create response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecCreated {
    #[serde(rename = "Id")]
    pub id: String,
}

/// Docker exec-start request.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ExecStart {
    #[serde(default, rename = "Detach")]
    pub detach: bool,
    #[serde(default, rename = "Tty")]
    pub tty: bool,
    /// Extension: terminate and remove this exec when its attached client disappears unexpectedly.
    #[serde(default, rename = "KillOnDisconnect")]
    pub kill_on_disconnect: bool,
    #[serde(default, rename = "ConsoleSize", skip_serializing_if = "Option::is_none")]
    pub console_size: Option<[u64; 2]>,
    #[serde(flatten, default)]
    pub unsupported: std::collections::BTreeMap<String, serde_json::Value>,
}

/// Husklet extension for reconnecting streams to an already-running exec.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ExecAttach {
    #[serde(default, rename = "Tty")]
    pub tty: bool,
    #[serde(default, rename = "KillOnDisconnect")]
    pub kill_on_disconnect: bool,
    #[serde(default, rename = "ConsoleSize", skip_serializing_if = "Option::is_none")]
    pub console_size: Option<[u64; 2]>,
}

#[cfg(feature = "runtime")]
impl ExecAttach {
    pub(crate) fn size(&self) -> Result<Option<hl_container::Size>, String> {
        let size = console_size(self.console_size)?;
        if size.is_some() && !self.tty {
            return Err("ConsoleSize requires Tty=true".into());
        }
        Ok(size)
    }
}

#[cfg(feature = "runtime")]
impl ExecStart {
    pub(crate) fn size(&self) -> Result<Option<hl_container::Size>, String> {
        if let Some(name) = CompatibilityFields::from(&self.unsupported).first_meaningful() {
            return Err(format!("unsupported exec start field {name}"));
        }
        let size = console_size(self.console_size)?;
        if size.is_some() && !self.tty {
            return Err("ConsoleSize requires Tty=true".into());
        }
        Ok(size)
    }
}

/// Stream availability reported by exec inspect.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct ExecOpen {
    #[serde(rename = "OpenStdin")]
    pub stdin: bool,
    #[serde(rename = "OpenStdout")]
    pub stdout: bool,
    #[serde(rename = "OpenStderr")]
    pub stderr: bool,
}

/// Process metadata reported by exec inspect.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ExecProcess {
    pub arguments: Vec<String>,
    pub entrypoint: String,
    pub privileged: bool,
    pub tty: bool,
    pub user: String,
}

/// Docker exec inspection response.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExecInspect {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "ContainerID")]
    pub container_id: String,
    #[serde(rename = "Running")]
    pub running: bool,
    #[serde(rename = "ExitCode")]
    pub exit_code: i64,
    #[serde(rename = "Pid")]
    pub pid: i64,
    #[serde(rename = "CanRemove")]
    pub can_remove: bool,
    #[serde(rename = "DetachKeys")]
    pub detach_keys: String,
    #[serde(flatten)]
    pub open: ExecOpen,
    #[serde(rename = "ProcessConfig")]
    pub process: ExecProcess,
}

/// Standard-input and terminal settings for the initial process.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct Console {
    #[serde(default, rename = "OpenStdin")]
    pub open: bool,
    #[serde(default, rename = "StdinOnce")]
    pub once: bool,
    #[serde(default, rename = "Tty")]
    pub tty: bool,
}

#[cfg(test)]
mod tests {
    use super::{Attachment, ExecConfig, ExecLifetime, ExecNetwork, ExecStart};

    #[test]
    fn exec_wire_models_use_docker_field_names() {
        let config = ExecConfig {
            attach: Attachment {
                stdin: true,
                stdout: true,
                stderr: false,
            },
            command: vec!["sh".into(), "-c".into(), "echo ok".into()],
            ..ExecConfig::default()
        };
        assert_eq!(
            serde_json::to_value(config).unwrap(),
            serde_json::json!({
                "AttachStdin": true,
                "AttachStdout": true,
                "AttachStderr": false,
                "DetachKeys": "",
                "Tty": false,
                "Cmd": ["sh", "-c", "echo ok"],
                "Privileged": false,
                "User": "",
                "WorkingDir": ""
            })
        );
        assert_eq!(
            serde_json::to_value(ExecStart::default()).unwrap(),
            serde_json::json!({"Detach": false, "Tty": false, "KillOnDisconnect": false})
        );
    }

    #[test]
    fn ephemeral_native_fields_are_explicit_and_legacy_payloads_keep_persisted_defaults() {
        let legacy: ExecConfig = serde_json::from_value(serde_json::json!({"Cmd": ["/bin/true"]})).unwrap();
        assert_eq!(legacy.lifetime, ExecLifetime::Persisted);
        assert_eq!(legacy.network, ExecNetwork::Container);
        assert!(!legacy.native);

        let encoded = serde_json::to_value(ExecConfig {
            command: vec!["/bin/true".into()],
            lifetime: ExecLifetime::Ephemeral,
            network: ExecNetwork::Isolated,
            native: true,
            ..ExecConfig::default()
        })
        .unwrap();
        assert_eq!(encoded["HuskletLifetime"], "ephemeral");
        assert_eq!(encoded["HuskletNetwork"], "isolated");
        assert_eq!(encoded["HuskletNative"], true);

        let live = serde_json::to_value(ExecConfig {
            lifetime: ExecLifetime::Live,
            ..ExecConfig::default()
        })
        .unwrap();
        assert_eq!(live["HuskletLifetime"], "live");
    }

    #[cfg(feature = "runtime")]
    #[test]
    fn exec_url_argument_round_trips_without_path_normalization() {
        const URL: &str = "https://google.com";
        let encoded = serde_json::to_vec(&ExecConfig {
            command: vec!["google-chrome".into(), URL.into()],
            ..ExecConfig::default()
        })
        .unwrap();
        let decoded: ExecConfig = serde_json::from_slice(&encoded).unwrap();

        assert_eq!(decoded.command, ["google-chrome", URL]);
        let process = decoded.process(&hl_container::Process::new("/bin/true")).unwrap();
        assert_eq!(process.program, "google-chrome");
        assert_eq!(process.args, [URL]);
    }

    #[cfg(feature = "runtime")]
    #[test]
    fn rejects_meaningful_options_the_runtime_cannot_honor() {
        let parent = hl_container::Process::new("/bin/true");
        let mut config = ExecConfig {
            command: vec!["/bin/true".into()],
            privileged: true,
            ..ExecConfig::default()
        };
        assert_eq!(
            config.process(&parent).unwrap_err(),
            "exec Privileged is not implemented"
        );
        config.privileged = false;
        config.detach_keys = "ctrl-x,x".into();
        assert!(config.process(&parent).is_ok());

        let unknown: ExecConfig = serde_json::from_value(serde_json::json!({
            "Cmd": ["/bin/true"],
            "FutureNamespace": "shared"
        }))
        .unwrap();
        assert_eq!(
            unknown.process(&parent).unwrap_err(),
            "unsupported exec create field FutureNamespace"
        );

        let start: ExecStart = serde_json::from_value(serde_json::json!({
            "FutureConsoleMode": "raw"
        }))
        .unwrap();
        assert_eq!(
            start.size().unwrap_err(),
            "unsupported exec start field FutureConsoleMode"
        );
    }

    /// Docker accepts `user`, `user:group`, `uid`, `uid:gid` and mixed forms. Resolving them needs the
    /// container's account databases, so the transport must forward the field verbatim.
    #[cfg(feature = "runtime")]
    #[test]
    fn exec_user_field_is_forwarded_for_container_side_resolution() {
        let parent = hl_container::Process::new("/bin/true").user(11, 12);
        for user in [
            "root",
            "ubuntu",
            "ubuntu:sudo",
            "1000:sudo",
            "ubuntu:1000",
            "0",
            "1000:1000",
            "definitely-not-a-user",
            ":",
        ] {
            let config = ExecConfig {
                command: vec!["/bin/true".into()],
                user: user.to_owned(),
                ..ExecConfig::default()
            };
            let process = config
                .process(&parent)
                .unwrap_or_else(|error| panic!("exec User {user:?} rejected at transport: {error}"));
            assert_eq!(
                (process.uid, process.gid),
                (Some(11), Some(12)),
                "transport must not decide identity for exec User {user:?}"
            );
        }
    }

    #[cfg(feature = "runtime")]
    #[test]
    fn console_size_is_height_then_width_and_rejects_zero() {
        let size = ExecStart {
            tty: true,
            console_size: Some([41, 109]),
            ..Default::default()
        }
        .size()
        .unwrap()
        .unwrap();
        assert_eq!((size.rows(), size.columns()), (41, 109));
        assert_eq!(
            ExecStart {
                tty: true,
                console_size: Some([0, 0]),
                ..Default::default()
            }
            .size()
            .unwrap(),
            None
        );
        assert!(
            ExecStart {
                tty: true,
                console_size: Some([0, 109]),
                ..Default::default()
            }
            .size()
            .is_err()
        );
        assert_eq!(
            ExecStart {
                tty: false,
                console_size: Some([41, 109]),
                ..Default::default()
            }
            .size()
            .unwrap_err(),
            "ConsoleSize requires Tty=true"
        );
    }
}
