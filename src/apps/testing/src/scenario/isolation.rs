use super::definition::{Resource, Sample};
use hl_container::{Isolation, Sandbox};

pub(super) fn for_case(case: &Sample) -> Isolation {
    Isolation {
        sandbox: Sandbox::Disabled,
        network_isolated: case.resources.contains(&Resource::HostPort),
        ..Isolation::default()
    }
}

#[cfg(test)]
mod tests {
    use super::for_case;
    use crate::{
        scenario::definition::{Class, Resource, Sample, Step},
        suite::{Execution, Target},
    };
    use std::collections::BTreeMap;

    fn scenario(resources: Vec<Resource>) -> Sample {
        Sample {
            id: "network/probe".to_owned(),
            image: "alpine".to_owned(),
            execution: Execution::default(),
            class: Class::Quick,
            targets: vec![Target::Arm64, Target::Amd64],
            expected_failures: Vec::new(),
            resources,
            environment: BTreeMap::new(),
            working_directory: "/".to_owned(),
            actions: vec![Step::Shell("true".to_owned())],
            fixtures: Vec::new(),
            readiness: None,
            timeout: 60,
            warmups: 0,
            repetitions: 1,
            exit: 0,
            stdout_contains: Vec::new(),
            stdout_exact: None,
            output_empty: false,
        }
    }

    #[test]
    fn host_port_selects_network_isolation() {
        assert!(for_case(&scenario(vec![Resource::HostPort])).network_isolated);
        assert!(!for_case(&scenario(Vec::new())).network_isolated);
    }
}
