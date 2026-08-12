use std::ffi::OsString;

const TEST_FORCE_DISPLACED_ET_EXEC: &str = "HL_TEST_FORCE_DISPLACED_ET_EXEC";

pub(super) fn worker_environment(get: impl Fn(&str) -> Option<OsString>) -> Vec<(&'static str, OsString)> {
    [
        hl_log::LOG_TAGS,
        hl_log::LOG_LEVEL,
        hl_log::PROFILE_TAGS,
        TEST_FORCE_DISPLACED_ET_EXEC,
    ]
    .into_iter()
    .filter_map(|name| get(name).map(|value| (name, value)))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::worker_environment;

    #[test]
    fn worker_environment_forwards_structured_controls_and_test_placement() {
        let environment = worker_environment(|name| Some(format!("value-for-{name}").into()));
        assert_eq!(
            environment.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            [
                hl_log::LOG_TAGS,
                hl_log::LOG_LEVEL,
                hl_log::PROFILE_TAGS,
                super::TEST_FORCE_DISPLACED_ET_EXEC,
            ]
        );
        assert!(environment.iter().all(|(_, value)| !value.is_empty()));
    }
}
