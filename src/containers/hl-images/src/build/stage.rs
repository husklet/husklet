use std::collections::BTreeMap;

use crate::{Error, History, Platform, Result, RuntimeConfig};

use super::instruction::{Assignments, Words, WorkingDirectory};
use super::model::{Base, CacheSharing, CopySource, OwnershipSpec, RunMount, Source, Stage, Step};
use super::value::{Command, Environment, Healthcheck};

pub(super) struct Draft {
    index: usize,
    pub(super) stage: Stage,
    pub(super) arguments: BTreeMap<String, String>,
    shell: Vec<String>,
}

impl Draft {
    pub(super) fn new(index: usize, name: Option<String>, base: Base, platform: Option<Platform>) -> Self {
        Self {
            index,
            stage: Stage {
                name,
                base,
                platform,
                runtime: RuntimeConfig {
                    entrypoint: Vec::new(),
                    command: Vec::new(),
                    environment: BTreeMap::new(),
                    working_directory: "/".into(),
                    user: String::new(),
                },
                command: None,
                entrypoint: None,
                working_directory: None,
                user: None,
                labels: BTreeMap::new(),
                history: Vec::new(),
                onbuild: Vec::new(),
                exposed_ports: std::collections::BTreeSet::new(),
                volumes: std::collections::BTreeSet::new(),
                healthcheck: None,
                stop_signal: None,
                steps: Vec::new(),
            },
            arguments: BTreeMap::new(),
            shell: vec!["/bin/sh".into(), "-c".into()],
        }
    }

    pub(super) fn finish(self) -> Stage {
        self.stage
    }

    pub(super) fn apply(&mut self, name: &str, raw: &str, names: &BTreeMap<String, usize>) -> Result<()> {
        let mut variables = self.arguments.clone();
        variables.extend(self.stage.runtime.environment.clone());
        let expanded = Self::expand(name, raw, &variables)?;
        match name {
            "ENV" => self
                .stage
                .runtime
                .environment
                .extend(Assignments::new(&expanded).parse()?),
            "RUN" => self.run(&expanded, variables, names)?,
            "COPY" | "ADD" => self.copy(name, &expanded, names)?,
            "WORKDIR" => {
                self.stage.runtime.working_directory =
                    WorkingDirectory::new(&self.stage.runtime.working_directory).resolve(&expanded)?;
                self.stage.working_directory = Some(self.stage.runtime.working_directory.clone());
            }
            "CMD" => {
                self.stage.runtime.command = expanded.parse::<Command>()?.into();
                self.stage.command = Some(self.stage.runtime.command.clone());
            }
            "ENTRYPOINT" => {
                self.stage.runtime.entrypoint = expanded.parse::<Command>()?.into();
                self.stage.entrypoint = Some(self.stage.runtime.entrypoint.clone());
            }
            "SHELL" => {
                self.shell = serde_json::from_str(&expanded)
                    .map_err(|error| Error::MalformedOci(format!("invalid SHELL: {error}")))?;
                if self.shell.is_empty() {
                    return Err(Error::MalformedOci("SHELL must not be empty".into()));
                }
            }
            "USER" => {
                self.stage.runtime.user = expanded;
                self.stage.user = Some(self.stage.runtime.user.clone());
            }
            "LABEL" => self.stage.labels.extend(Assignments::new(&expanded).parse()?),
            "ONBUILD" => {
                let instruction = expanded.trim();
                let forbidden = instruction
                    .split_whitespace()
                    .next()
                    .is_some_and(|name| matches!(name.to_ascii_uppercase().as_str(), "FROM" | "ONBUILD"));
                if instruction.is_empty() || forbidden {
                    return Err(Error::MalformedOci("invalid ONBUILD trigger".into()));
                }
                self.stage.onbuild.push(instruction.into());
            }
            "EXPOSE" => {
                for port in Words::new(&expanded).parse() {
                    self.stage.exposed_ports.insert(if port.contains('/') {
                        port
                    } else {
                        format!("{port}/tcp")
                    });
                }
            }
            "VOLUME" => {
                let volumes = if expanded.trim_start().starts_with('[') {
                    serde_json::from_str(&expanded)
                        .map_err(|error| Error::MalformedOci(format!("invalid VOLUME: {error}")))?
                } else {
                    Words::new(&expanded).parse()
                };
                self.stage.volumes.extend(volumes);
            }
            "HEALTHCHECK" => {
                self.stage.healthcheck = Some(expanded.parse::<Healthcheck>()?.into());
            }
            "STOPSIGNAL" => {
                let signal = expanded.trim();
                if signal.is_empty() || signal.contains(char::is_whitespace) {
                    return Err(Error::MalformedOci("invalid STOPSIGNAL".into()));
                }
                self.stage.stop_signal = Some(signal.into());
            }
            other => {
                return Err(Error::MalformedOci(format!(
                    "Dockerfile instruction {other} is not supported"
                )));
            }
        }
        self.stage
            .history
            .push(History::instruction(name, raw, !matches!(name, "RUN" | "COPY" | "ADD")));
        Ok(())
    }

    fn expand(name: &str, raw: &str, variables: &BTreeMap<String, String>) -> Result<String> {
        if matches!(
            name,
            "ENV"
                | "WORKDIR"
                | "COPY"
                | "ADD"
                | "USER"
                | "LABEL"
                | "ONBUILD"
                | "EXPOSE"
                | "VOLUME"
                | "HEALTHCHECK"
                | "STOPSIGNAL"
        ) {
            Environment::new(variables).expand(raw)
        } else {
            Ok(raw.to_owned())
        }
    }

    fn copy(&mut self, name: &str, value: &str, names: &BTreeMap<String, usize>) -> Result<()> {
        let mut words = Words::new(value).parse();
        let mut from = None;
        let mut mode = None;
        let mut ownership = None;
        let mut ownership_options = 0_u8;
        let mut excludes = Vec::new();
        let (mut parents, mut link) = (false, false);
        let mut checksum = None;
        words.retain(|word| {
            if let Some(value) = word.strip_prefix("--from=") {
                from = names
                    .get(value)
                    .copied()
                    .or_else(|| value.parse::<usize>().ok().filter(|index| *index < self.index))
                    .map(CopySource::Stage)
                    .or_else(|| value.parse().ok().map(CopySource::Image));
                false
            } else if let Some(value) = word.strip_prefix("--chmod=") {
                mode = u32::from_str_radix(value.trim_start_matches("0o"), 8)
                    .ok()
                    .filter(|mode| *mode <= 0o7777);
                false
            } else if let Some(value) = word.strip_prefix("--chown=") {
                ownership_options = ownership_options.saturating_add(1);
                ownership = value.parse().ok();
                false
            } else if let Some(value) = word.strip_prefix("--exclude=") {
                let value = value.trim_start_matches('/').trim_end_matches('/');
                if !value.is_empty() {
                    excludes.push(value.to_owned());
                }
                false
            } else if word == "--parents" || word == "--parents=true" {
                parents = true;
                false
            } else if word == "--parents=false" || word == "--link=false" {
                false
            } else if word == "--link" || word == "--link=true" {
                link = true;
                false
            } else if let Some(value) = word.strip_prefix("--checksum=") {
                checksum = Some(value.to_owned());
                false
            } else {
                true
            }
        });
        if words.iter().any(|word| word.starts_with("--")) {
            return Err(Error::MalformedOci("unsupported COPY/ADD option".into()));
        }
        CopyOptions {
            value,
            link,
            mode,
            ownership_options,
            ownership: ownership.as_ref(),
            excludes: &excludes,
        }
        .validate()?;
        if words.len() < 2 {
            return Err(Error::MalformedOci(format!("{name} requires sources and target")));
        }
        if value.contains("--from=") && from.is_none() {
            return Err(Error::MalformedOci(
                "COPY --from requires a valid prior stage or image reference".into(),
            ));
        }
        let target = words.pop().expect("at least two words");
        if words.len() > 1 && !target.ends_with('/') {
            return Err(Error::MalformedOci("multi-source COPY target must end with '/'".into()));
        }
        if parents && !target.ends_with('/') {
            return Err(Error::MalformedOci(
                "COPY/ADD --parents target must end with '/'".into(),
            ));
        }
        let sources = words.into_iter().map(Source::from).collect::<Vec<_>>();
        Sources(&sources).validate(name, from.as_ref(), checksum.as_deref())?;
        self.stage.steps.push(Step::Copy {
            sources,
            target,
            directory: self.stage.working_directory.clone(),
            from,
            unpack: name == "ADD",
            mode,
            ownership,
            excludes,
            parents,
            checksum,
        });
        Ok(())
    }

    fn run(&mut self, value: &str, variables: BTreeMap<String, String>, names: &BTreeMap<String, usize>) -> Result<()> {
        let mut command = value.trim_start();
        let mut mounts = Vec::new();
        while let Some(value) = command.strip_prefix("--mount=") {
            let (option, rest) = value
                .split_once(char::is_whitespace)
                .ok_or_else(|| Error::MalformedOci("RUN --mount requires a command".into()))?;
            mounts.push(RunMount::parse(option, names, self.index)?);
            command = rest.trim_start();
        }
        if command.starts_with("--") {
            return Err(Error::MalformedOci("unsupported RUN option".into()));
        }
        if command.is_empty() {
            return Err(Error::MalformedOci("RUN requires a command".into()));
        }
        self.stage.steps.push(Step::Run {
            command: command.into(),
            environment: variables,
            directory: self.stage.working_directory.clone(),
            shell: self.shell.clone(),
            user: self.stage.user.clone(),
            mounts,
        });
        Ok(())
    }
}

struct CopyOptions<'a> {
    value: &'a str,
    link: bool,
    mode: Option<u32>,
    ownership_options: u8,
    ownership: Option<&'a OwnershipSpec>,
    excludes: &'a [String],
}

impl CopyOptions<'_> {
    fn validate(&self) -> Result<()> {
        if self.link {
            return Err(Error::MalformedOci(
                "COPY/ADD --link=true requires independent layer support".into(),
            ));
        }
        if self.value.contains("--chmod=") && self.mode.is_none() {
            return Err(Error::MalformedOci("invalid COPY/ADD --chmod".into()));
        }
        if self.ownership_options > 1 || (self.ownership_options == 1 && self.ownership.is_none()) {
            return Err(Error::MalformedOci(
                "COPY/ADD --chown requires uid/name[:gid/group]".into(),
            ));
        }
        if self.value.contains("--exclude=") && self.excludes.is_empty() {
            return Err(Error::MalformedOci("COPY/ADD --exclude requires a pattern".into()));
        }
        Ok(())
    }
}

struct Sources<'a>(&'a [Source]);

impl Sources<'_> {
    fn validate(&self, name: &str, from: Option<&CopySource>, checksum: Option<&str>) -> Result<()> {
        let remote = self.0.iter().filter(|source| source.is_remote()).count();
        if name == "COPY" && remote > 0 {
            return Err(Error::MalformedOci("COPY does not support remote URL sources".into()));
        }
        if remote > 0 && from.is_some() {
            return Err(Error::MalformedOci("remote ADD cannot use --from".into()));
        }
        if self.0.iter().any(|source| matches!(source, Source::Git(_))) {
            return Err(Error::MalformedOci("ADD Git sources are not supported".into()));
        }
        if let Some(value) = checksum {
            let digest = value
                .strip_prefix("sha256:")
                .ok_or_else(|| Error::MalformedOci("ADD --checksum supports only sha256".into()))?;
            if name != "ADD"
                || remote != 1
                || self.0.len() != 1
                || digest.len() != 64
                || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(Error::MalformedOci(
                    "ADD --checksum requires one remote URL and sha256:<64 hex>".into(),
                ));
            }
        }
        Ok(())
    }
}

impl RunMount {
    fn parse(value: &str, names: &BTreeMap<String, usize>, stage: usize) -> Result<Self> {
        let mut options = BTreeMap::new();
        for option in value.split(',') {
            let (name, value) = option.split_once('=').unwrap_or((option, "true"));
            if options.insert(name, value).is_some() {
                return Err(Error::MalformedOci(format!("duplicate RUN --mount option {name}")));
            }
        }
        let kind = options.remove("type").unwrap_or("bind");
        let target = options
            .remove("target")
            .or_else(|| options.remove("dst"))
            .or_else(|| options.remove("destination"))
            .ok_or_else(|| Error::MalformedOci("RUN --mount requires target".into()))?;
        let target_path = std::path::Path::new(target);
        if !target_path.is_absolute()
            || target_path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
        {
            return Err(Error::MalformedOci(
                "RUN --mount target must be a confined absolute path".into(),
            ));
        }
        match kind {
            "cache" => Self::cache(target, &mut options),
            "bind" => Self::bind(target, &mut options, names, stage),
            "secret" | "ssh" | "tmpfs" => Err(Error::MalformedOci(format!("RUN --mount=type={kind} is not supported"))),
            _ => Err(Error::MalformedOci(format!("unsupported RUN mount type {kind:?}"))),
        }
    }

    fn cache(target: &str, options: &mut BTreeMap<&str, &str>) -> Result<Self> {
        let sharing = match options.remove("sharing").unwrap_or("shared") {
            "shared" => CacheSharing::Shared,
            "locked" => CacheSharing::Locked,
            "private" => CacheSharing::Private,
            value => return Err(Error::MalformedOci(format!("unsupported cache sharing {value:?}"))),
        };
        let id = options.remove("id").map(str::to_owned);
        Self::reject_options(options)?;
        Ok(Self::Cache {
            id,
            target: target.into(),
            sharing,
        })
    }

    fn bind(
        target: &str,
        options: &mut BTreeMap<&str, &str>,
        names: &BTreeMap<String, usize>,
        stage: usize,
    ) -> Result<Self> {
        if options.remove("rw").is_some() || options.remove("readwrite").is_some() {
            return Err(Error::MalformedOci("writable RUN bind mounts are not supported".into()));
        }
        for name in ["ro", "readonly"] {
            if options.remove(name).is_some_and(|value| value != "true") {
                return Err(Error::MalformedOci(format!("RUN bind {name} must be true")));
            }
        }
        let source = options
            .remove("source")
            .or_else(|| options.remove("src"))
            .unwrap_or(".")
            .to_owned();
        let from = options
            .remove("from")
            .map(|value| {
                names
                    .get(value)
                    .copied()
                    .or_else(|| value.parse().ok().filter(|index| *index < stage))
                    .ok_or_else(|| Error::MalformedOci("RUN bind source stage was not found".into()))
            })
            .transpose()?;
        Self::reject_options(options)?;
        Ok(Self::Bind {
            from,
            source,
            target: target.into(),
        })
    }

    fn reject_options(options: &BTreeMap<&str, &str>) -> Result<()> {
        if let Some(name) = options.keys().next() {
            return Err(Error::MalformedOci(format!("unsupported RUN --mount option {name}")));
        }
        Ok(())
    }
}
