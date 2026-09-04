//! The packaged raw-rootfs worker must make its advertised persistent cache observable on disk.

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
#[ignore = "product test: runner supplies the packaged worker and immutable rootfs workload"]
fn raw_worker_publishes_and_reuses_its_translation_cache() {
    let worker = std::env::var_os("HL_TRANSLATION_CACHE_WORKER").expect("runner must name the packaged worker");
    let rootfs = std::env::var_os("HL_TRANSLATION_CACHE_ROOTFS").expect("runner must name the rootfs");
    let cache = tempfile::tempdir().unwrap();
    let run = |label: &str| {
        let output = std::process::Command::new(&worker)
            .args([
                "--rootfs",
                std::path::Path::new(&rootfs).to_str().unwrap(),
                "--translit",
                "--translation-cache",
                cache.path().to_str().unwrap(),
                "--translation-cache-observe",
                "bin/sh",
                "/work/session.sh",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{label}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stderr).unwrap()
    };
    let cold = run("cold");
    assert!(
        cold.contains("[pcache-v1] nested_exec=disabled bus_active=0"),
        "nested developer exec retained persistence's guarded-code mode: {cold}"
    );
    let assert_observed_without_nested_census = |label: &str, stderr: &str| {
        let nested = stderr
            .lines()
            .filter(|line| line.starts_with("[pcache-v1] nested_exec=disabled "))
            .collect::<Vec<_>>();
        assert!(
            nested.len() > 1,
            "{label}: fixture did not cross a second nested-exec boundary: {stderr}"
        );
        assert!(
            nested.iter().all(|line| line.ends_with(" post_disable_census_sites=0")),
            "{label}: observation emitted census instrumentation after launch-only disable: {nested:?}"
        );
        let refused = stderr
            .lines()
            .filter(|line| line.starts_with("[pcache] save refused "))
            .collect::<Vec<_>>();
        assert!(
            !refused.is_empty(),
            "{label}: fixture observed no nested process exit: {stderr}"
        );
        assert!(
            refused
                .iter()
                .all(|line| line.ends_with(" post_disable_census_sites=0")),
            "{label}: a nested process executed census-instrumented blocks after disable: {refused:?}"
        );
        assert!(
            stderr.contains("[pcache-v1] outcome="),
            "{label}: disabling nested census also disabled cache outcome observation: {stderr}"
        );
    };
    assert_observed_without_nested_census("cold", &cold);
    assert!(
        cache.path().read_dir().unwrap().next().is_some(),
        "cold run published no cache artifact: {cold}"
    );
    let artifacts = || {
        cache
            .path()
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "x64pcache")
            })
            .count()
    };
    assert_eq!(
        artifacts(),
        1,
        "nested developer tools published unauthorised cache artifacts"
    );
    let warm = run("warm");
    assert!(
        warm.contains("[pcache] HIT (translation skipped)"),
        "warm run did not reuse translated code: {warm}"
    );
    assert!(
        warm.contains("[pcache-v1] nested_exec=disabled bus_active=0"),
        "warm nested exec retained persistence's guarded-code mode: {warm}"
    );
    assert_observed_without_nested_census("warm", &warm);
    assert_eq!(
        artifacts(),
        1,
        "warm nested execs expanded the authenticated cache surface"
    );
}
