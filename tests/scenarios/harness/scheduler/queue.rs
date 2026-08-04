use super::Error;
use crate::contract::Resource;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Clone, Copy)]
pub(super) struct Task {
    pub(super) category: &'static str,
    pub(super) command: &'static str,
}

pub(super) struct Resources {
    disk: Arc<Semaphore>,
    registry: Arc<Semaphore>,
    host_port: Arc<Semaphore>,
    image_mutation: Arc<Semaphore>,
    network: Arc<Semaphore>,
    process_heavy: Arc<Semaphore>,
}

impl Resources {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            disk: Arc::new(Semaphore::new(2)),
            registry: Arc::new(Semaphore::new(2)),
            host_port: Arc::new(Semaphore::new(4)),
            image_mutation: Arc::new(Semaphore::new(1)),
            network: Arc::new(Semaphore::new(4)),
            process_heavy: Arc::new(Semaphore::new(1)),
        })
    }

    pub(super) async fn acquire(
        &self,
        resource: Resource,
    ) -> Result<Option<OwnedSemaphorePermit>, Error> {
        let semaphore = match resource {
            Resource::Pty => return Ok(None),
            Resource::DiskHeavy => &self.disk,
            Resource::Registry => &self.registry,
            Resource::Network => &self.network,
            Resource::HostPort => &self.host_port,
            Resource::ImageMutation => &self.image_mutation,
            Resource::ProcessHeavy => &self.process_heavy,
        };
        Ok(Some(semaphore.clone().acquire_owned().await?))
    }
}

pub(super) const TASKS: &[Task] = &[
    Task {
        category: "buildcmd",
        command: "buildcmd",
    },
    Task {
        category: "copy",
        command: "copy",
    },
    Task {
        category: "databases",
        command: "databases",
    },
    Task {
        category: "distros",
        command: "distros",
    },
    Task {
        category: "execcmd",
        command: "execcmd",
    },
    Task {
        category: "languages",
        command: "languages",
    },
    Task {
        category: "lifecycle",
        command: "lifecycle",
    },
    Task {
        category: "netcontainer",
        command: "netcontainer",
    },
    Task {
        category: "network",
        command: "network-contracts",
    },
    Task {
        category: "permissions",
        command: "permissions",
    },
    Task {
        category: "process",
        command: "process",
    },
    Task {
        category: "runflags",
        command: "runflags",
    },
    Task {
        category: "terminal",
        command: "terminal",
    },
    Task {
        category: "toolchains",
        command: "toolchains",
    },
    Task {
        category: "utilities",
        command: "utilities",
    },
    Task {
        category: "volume",
        command: "volume-contracts",
    },
    Task {
        category: "web",
        command: "web",
    },
    Task {
        category: "weird",
        command: "weird",
    },
];

pub(super) fn owns(task: &Task, id: &str) -> bool {
    let prefix = id.split_once('/').map_or(id, |(prefix, _)| prefix);
    match task.category {
        "copy" => matches!(prefix, "copy" | "cpcmd" | "cpcoherence"),
        "network" => matches!(prefix, "networking" | "netinstall" | "dockernet"),
        "volume" => matches!(prefix, "volume" | "volumes" | "dockervol"),
        category => prefix == category,
    }
}

pub(super) fn requirements(task: &Task, selected: Option<&str>) -> BTreeSet<Resource> {
    let registry = crate::registry::build();
    let declared = registry
        .scenarios()
        .filter(|scenario| owns(task, scenario.id))
        .filter(|scenario| selected.is_none_or(|id| scenario.id == id))
        .flat_map(|scenario| scenario.resources.iter().copied())
        .collect::<BTreeSet<_>>();
    if !declared.is_empty() {
        return declared;
    }
    fallback(task.category).into_iter().collect()
}

fn fallback(category: &str) -> Option<Resource> {
    match category {
        "buildcmd" | "distros" | "languages" | "toolchains" => Some(Resource::Registry),
        "weird" => Some(Resource::ProcessHeavy),
        "copy" | "volume" => Some(Resource::DiskHeavy),
        "databases" | "netcontainer" | "network" | "runflags" | "web" => Some(Resource::HostPort),
        _ => None,
    }
}

pub(super) fn test_requirements() -> Result<(), Error> {
    let registry = crate::registry::build();
    let mut owned = BTreeMap::<&str, Vec<&str>>::new();
    for scenario in registry.scenarios() {
        for task in TASKS.iter().filter(|task| owns(task, scenario.id)) {
            owned.entry(scenario.id).or_default().push(task.category);
        }
    }
    let invalid = owned
        .iter()
        .filter(|(_, categories)| categories.len() != 1)
        .map(|(id, categories)| format!("{id} -> {categories:?}"))
        .collect::<Vec<_>>();
    if !invalid.is_empty() {
        return Err(format!(
            "every compatibility contract must have exactly one all-suite owner: {}",
            invalid.join(", ")
        )
        .into());
    }
    let registered = crate::registry::build().scenarios().count();
    if owned.len() != registered {
        return Err(format!(
            "all-suite owns {} of {registered} registered compatibility contracts",
            owned.len()
        )
        .into());
    }
    let empty_tasks = TASKS
        .iter()
        .filter(|task| {
            !owned
                .values()
                .any(|categories| categories.contains(&task.category))
        })
        .map(|task| task.category)
        .collect::<Vec<_>>();
    if !empty_tasks.is_empty() {
        return Err(format!(
            "all-suite tasks own no compatibility contracts: {}",
            empty_tasks.join(", ")
        )
        .into());
    }
    if Resources::new().process_heavy.available_permits() != 1 {
        return Err("process-heavy scenarios must share one global compiler budget".into());
    }
    let languages = TASKS
        .iter()
        .find(|task| task.category == "languages")
        .ok_or("languages scheduler task is missing")?;
    let sdk = requirements(languages, Some("languages/dotnet-sum-sdk8"));
    let expected = BTreeSet::from([Resource::DiskHeavy, Resource::Registry]);
    if sdk != expected {
        return Err(format!("dotnet scheduler resources {sdk:?}, expected {expected:?}").into());
    }
    let distro = TASKS
        .iter()
        .find(|task| task.category == "distros")
        .ok_or("distros scheduler task is missing")?;
    if requirements(distro, Some("distros/alpine-sed")) != BTreeSet::from([Resource::Registry]) {
        return Err("selected manifest resources did not constrain scheduling".into());
    }
    let apt = requirements(distro, Some("distros/debian-apt-update"));
    let expected = BTreeSet::from([Resource::DiskHeavy, Resource::Network, Resource::Registry]);
    if apt != expected {
        return Err(format!("apt scheduler resources {apt:?}, expected {expected:?}").into());
    }
    let go = requirements(languages, Some("languages/go-sum-122-alpine"));
    if go != BTreeSet::from([Resource::ProcessHeavy]) {
        return Err(format!("Go scheduler resources {go:?}, expected process_heavy").into());
    }
    let rust = requirements(languages, Some("languages/rust-sum-1-alpine"));
    if rust != BTreeSet::from([Resource::ProcessHeavy]) {
        return Err(format!("Rust scheduler resources {rust:?}, expected process_heavy").into());
    }
    let toolchains = TASKS
        .iter()
        .find(|task| task.category == "toolchains")
        .ok_or("toolchains scheduler task is missing")?;
    if requirements(toolchains, Some("toolchains/go-123-run-sum"))
        != BTreeSet::from([Resource::ProcessHeavy])
    {
        return Err("compiler scenarios must declare the process-heavy budget".into());
    }
    Ok(())
}
