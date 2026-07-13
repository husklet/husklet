//! A pulled image: its unpacked rootfs, detected arch, config blob, and the accessors over it.

use super::*;
use crate::registry::ImageRef;
use serde_json::Value;
use std::path::PathBuf;

/// An image pulled into the local store: its unpacked `rootfs`, detected [`Arch`], and OCI config blob.
/// Hand `rootfs` + `arch` to your runtime; read the image's default command/env with the accessors.
#[derive(Clone, Debug)]
pub struct LocalImage {
    /// The unpacked root filesystem — hand this to your runtime.
    pub rootfs: PathBuf,
    /// The target (OS + ISA) detected from the image config.
    pub arch: Arch,
    /// The raw OCI image config blob (`{ architecture, os, config: { Cmd, Entrypoint, Env, … } }`).
    pub config: Value,
    /// The reference this was pulled from.
    pub iref: ImageRef,
}

impl LocalImage {
    /// The image's `Entrypoint` (OCI `config.config.Entrypoint`).
    pub fn entrypoint(&self) -> Vec<String> {
        config_strs(&self.config, "Entrypoint")
    }

    /// The image's default `Cmd` (OCI `config.config.Cmd`).
    pub fn cmd(&self) -> Vec<String> {
        config_strs(&self.config, "Cmd")
    }

    /// The image's `Env` lines (`K=V`).
    pub fn env(&self) -> Vec<String> {
        config_strs(&self.config, "Env")
    }

    /// The image's `WorkingDir` (empty if unset).
    pub fn workdir(&self) -> String {
        self.config["config"]["WorkingDir"].as_str().unwrap_or("").to_string()
    }

    /// The image's default `User` (empty if unset).
    pub fn user(&self) -> String {
        self.config["config"]["User"].as_str().unwrap_or("").to_string()
    }

    /// The effective launch argv: the image `Entrypoint` followed by `override_cmd` if non-empty, else
    /// the image's own `Cmd` (docker's entrypoint/cmd composition). Pass the result to `.cmd(..)`.
    pub fn entrypoint_cmd<S: Into<String>>(&self, override_cmd: impl IntoIterator<Item = S>) -> Vec<String> {
        let mut argv = self.entrypoint();
        let over: Vec<String> = override_cmd.into_iter().map(Into::into).collect();
        if over.is_empty() {
            argv.extend(self.cmd());
        } else {
            argv.extend(over);
        }
        argv
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn image(entrypoint: &[&str], cmd: &[&str]) -> LocalImage {
        LocalImage {
            rootfs: PathBuf::from("/tmp/rootfs"),
            arch: Arch::LinuxAarch64,
            config: json!({ "config": { "Entrypoint": entrypoint, "Cmd": cmd } }),
            iref: ImageRef::parse("nginx"),
        }
    }

    #[test]
    fn entrypoint_plus_image_cmd_when_no_override() {
        // entrypoint set + empty override -> entrypoint ++ image Cmd
        let img = image(&["/entry"], &["arg1", "arg2"]);
        let empty: Vec<String> = Vec::new();
        assert_eq!(img.entrypoint_cmd(empty), vec!["/entry", "arg1", "arg2"]);
    }

    #[test]
    fn entrypoint_plus_override_drops_image_cmd() {
        // entrypoint set + non-empty override -> entrypoint ++ override (image Cmd dropped)
        let img = image(&["/entry"], &["arg1", "arg2"]);
        assert_eq!(img.entrypoint_cmd(["run", "now"]), vec!["/entry", "run", "now"]);
    }

    #[test]
    fn override_alone_when_no_entrypoint() {
        // no entrypoint + override -> override alone
        let img = image(&[], &["arg1"]);
        assert_eq!(img.entrypoint_cmd(["only"]), vec!["only"]);
    }

    #[test]
    fn image_cmd_alone_when_no_entrypoint_no_override() {
        // no entrypoint + no override -> image Cmd alone
        let img = image(&[], &["default", "cmd"]);
        let empty: Vec<String> = Vec::new();
        assert_eq!(img.entrypoint_cmd(empty), vec!["default", "cmd"]);
    }
}
