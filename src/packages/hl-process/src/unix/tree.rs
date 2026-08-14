//! Linux descendant discovery for processes that escape the owned group.

#![allow(unsafe_code)]

use super::{POLL, TERM_GRACE};
use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::thread;
use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Identity {
    process: i32,
    started: u64,
}

pub(super) struct Descendants(Vec<Identity>);

impl Descendants {
    pub(super) fn freeze(root: u32) -> std::io::Result<Self> {
        let root =
            i32::try_from(root).map_err(|_| std::io::Error::other("subprocess identity exceeded host pid range"))?;
        let mut pending = VecDeque::from([root]);
        let mut seen = BTreeSet::from([root]);
        let mut descendants = Vec::new();
        while let Some(parent) = pending.pop_front() {
            for process in children(parent)? {
                if !seen.insert(process) {
                    continue;
                }
                let Some(identity) = identity(process)? else {
                    continue;
                };
                identity.signal(libc::SIGSTOP)?;
                if identity.exists() {
                    descendants.push(identity);
                    pending.push_back(process);
                }
            }
        }
        Ok(Self(descendants))
    }

    pub(super) fn signal(&self, signal: i32) -> std::io::Result<()> {
        for identity in &self.0 {
            identity.signal(signal)?;
        }
        Ok(())
    }

    pub(super) fn exists(&self) -> bool {
        self.0.iter().any(|identity| identity.exists())
    }

    pub(super) fn settle(&self) -> bool {
        let deadline = Instant::now() + TERM_GRACE;
        while self.exists() {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(POLL);
        }
        true
    }
}

impl Identity {
    fn signal(self, signal: i32) -> std::io::Result<()> {
        if !self.exists() {
            return Ok(());
        }
        // SAFETY: the positive PID and scalar signal refer to no Rust storage.
        // The start-time check above prevents signalling a recycled identity.
        if unsafe { libc::kill(self.process, signal) } == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }

    fn exists(self) -> bool {
        matches!(identity(self.process), Ok(Some(current)) if current == self && !zombie(self.process))
    }
}

fn children(process: i32) -> std::io::Result<Vec<i32>> {
    let tasks = match fs::read_dir(format!("/proc/{process}/task")) {
        Ok(tasks) => tasks,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut children = BTreeSet::new();
    for task in tasks {
        let task = task?;
        let text = match fs::read_to_string(task.path().join("children")) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for value in text.split_ascii_whitespace() {
            children.insert(
                value.parse().map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid process child identity")
                })?,
            );
        }
    }
    Ok(children.into_iter().collect())
}

fn identity(process: i32) -> std::io::Result<Option<Identity>> {
    let Some(stat) = stat(process)? else {
        return Ok(None);
    };
    let Some((_, fields)) = stat.rsplit_once(") ") else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid process stat record",
        ));
    };
    let started = fields
        .split_ascii_whitespace()
        .nth(19)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "process stat omitted start time"))?
        .parse()
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid process start time"))?;
    Ok(Some(Identity { process, started }))
}

fn zombie(process: i32) -> bool {
    stat(process)
        .ok()
        .flatten()
        .and_then(|stat| stat.rsplit_once(") ").map(|(_, fields)| fields.starts_with('Z')))
        .unwrap_or(false)
}

fn stat(process: i32) -> std::io::Result<Option<String>> {
    match fs::read_to_string(format!("/proc/{process}/stat")) {
        Ok(stat) => Ok(Some(stat)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
