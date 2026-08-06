use std::collections::BTreeMap;

use crate::{Error, History, Platform, Result};

mod instruction;
mod model;
mod stage;
mod value;

#[cfg(test)]
use instruction::{Assignments, WorkingDirectory};
use instruction::{InstructionParser, Words};
pub use model::{Account, Base, CacheSharing, CopySource, OwnershipSpec, Recipe, RunMount, Source, Stage, Step};
use stage::Draft;
pub use value::Changes;
use value::Environment;
#[cfg(test)]
use value::{Command, Duration, Healthcheck};

impl Recipe {
    /// Parse a Dockerfile without externally supplied build arguments or a target stage.
    ///
    /// # Errors
    /// Returns an error when instructions, substitutions, stage references, or values are invalid.
    pub fn parse(text: &str) -> Result<Self> {
        Self::parse_with(text, &BTreeMap::new(), None)
    }

    /// Parse a Dockerfile using explicit build arguments and an optional target stage.
    ///
    /// # Errors
    /// Returns an error when instructions, substitutions, stage references, or values are invalid.
    pub fn parse_with(text: &str, supplied: &BTreeMap<String, String>, target: Option<&str>) -> Result<Self> {
        Self::parse_with_platforms(text, supplied, target, None, None)
    }

    /// Parse with Docker's predefined build and target platform arguments.
    ///
    /// # Errors
    /// Returns an error for malformed platform selectors or Dockerfile syntax.
    pub fn parse_with_platforms(
        text: &str,
        supplied: &BTreeMap<String, String>,
        target: Option<&str>,
        build_platform: Option<&Platform>,
        target_platform: Option<&Platform>,
    ) -> Result<Self> {
        let mut stages = Vec::new();
        let mut current: Option<Draft> = None;
        let mut names = BTreeMap::new();
        let mut globals = Self::platform_arguments(build_platform, target_platform);
        for instruction in InstructionParser::new(text).parse()? {
            let name = instruction.name;
            let raw = instruction.value;
            if name == "ARG" {
                let inherited = globals.clone();
                argument(&raw, supplied, &inherited, current.as_mut(), &mut globals)?;
                continue;
            }
            if name == "FROM" {
                if let Some(stage) = current.take() {
                    stages.push(stage.finish());
                }
                let value = Environment::new(&globals).expand(&raw)?;
                let mut words = Words::new(&value).parse();
                if words.is_empty() {
                    return Err(Error::MalformedOci("FROM requires an image".into()));
                }
                let platform = words
                    .first()
                    .and_then(|word| word.strip_prefix("--platform="))
                    .map(str::parse::<Platform>)
                    .transpose()
                    .map_err(|error| Error::MalformedOci(error.to_string()))?;
                if words.first().is_some_and(|word| word.starts_with("--platform=")) {
                    words.remove(0);
                }
                let name = if words.len() == 3 && words[1].eq_ignore_ascii_case("AS") {
                    Some(words[2].clone())
                } else if words.len() == 1 {
                    None
                } else {
                    return Err(Error::MalformedOci("invalid FROM syntax".into()));
                };
                let base = names
                    .get(&words[0])
                    .copied()
                    .or_else(|| words[0].parse::<usize>().ok().filter(|index| *index < stages.len()))
                    .map_or_else(|| words[0].parse().map(Base::Image), |index| Ok(Base::Stage(index)))?;
                let index = stages.len();
                if let Some(name) = &name
                    && names.insert(name.clone(), index).is_some() {
                        return Err(Error::MalformedOci(format!("duplicate stage {name}")));
                    }
                let mut draft = Draft::new(index, name, base, platform);
                draft.stage.history.push(History::instruction("FROM", &raw, true));
                current = Some(draft);
                continue;
            }
            let stage = current
                .as_mut()
                .ok_or_else(|| Error::MalformedOci(format!("{name} appears before FROM")))?;
            stage.apply(&name, &raw, &names)?;
        }
        if let Some(stage) = current {
            stages.push(stage.finish());
        }
        if stages.is_empty() {
            return Err(Error::MalformedOci("Dockerfile has no FROM".into()));
        }
        let selected = Self::selected_stage(target, &names, stages.len())?;
        Ok(Self { stages, selected })
    }

    fn platform_arguments(build: Option<&Platform>, target: Option<&Platform>) -> BTreeMap<String, String> {
        let mut arguments = BTreeMap::new();
        for (prefix, platform) in [("BUILD", build), ("TARGET", target)] {
            let Some(platform) = platform else { continue };
            arguments.insert(format!("{prefix}PLATFORM"), platform.to_string());
            arguments.insert(format!("{prefix}OS"), platform.os.clone());
            arguments.insert(format!("{prefix}ARCH"), platform.architecture.clone());
            arguments.insert(format!("{prefix}VARIANT"), platform.variant.clone().unwrap_or_default());
        }
        arguments
    }

    fn selected_stage(target: Option<&str>, names: &BTreeMap<String, usize>, stage_count: usize) -> Result<usize> {
        target.map_or(Ok(stage_count - 1), |target| {
            names
                .get(target)
                .copied()
                .or_else(|| target.parse().ok().filter(|index| *index < stage_count))
                .ok_or_else(|| Error::MalformedOci(format!("target stage {target:?} was not found")))
        })
    }
}

fn argument(
    raw: &str,
    supplied: &BTreeMap<String, String>,
    globals: &BTreeMap<String, String>,
    stage: Option<&mut Draft>,
    global_output: &mut BTreeMap<String, String>,
) -> Result<()> {
    let (key, default) = raw
        .split_once('=')
        .map_or((raw, None), |(key, value)| (key, Some(value)));
    if key.is_empty() || key.contains(char::is_whitespace) {
        return Err(Error::MalformedOci("invalid ARG name".into()));
    }
    let value = supplied
        .get(key)
        .cloned()
        .or_else(|| default.map(ToOwned::to_owned))
        .or_else(|| globals.get(key).cloned());
    if let Some(stage) = stage {
        if let Some(value) = value {
            stage.arguments.insert(key.into(), value);
        }
        stage.stage.history.push(History::instruction("ARG", raw, true));
    } else if let Some(value) = value {
        global_output.insert(key.into(), value);
    }
    Ok(())
}

impl History {
    fn instruction(name: &str, value: &str, empty_layer: bool) -> Self {
        Self {
            created: None,
            created_by: Some(format!("{name} {value}")),
            comment: None,
            empty_layer,
        }
    }

    fn change(value: &str) -> Self {
        Self {
            created: None,
            created_by: Some(value.into()),
            comment: None,
            empty_layer: true,
        }
    }
}

#[cfg(test)]
mod test;
