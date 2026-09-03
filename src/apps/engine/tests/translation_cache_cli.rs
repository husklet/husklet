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
        cache.path().read_dir().unwrap().next().is_some(),
        "cold run published no cache artifact: {cold}"
    );
    let artifacts = || {
        cache
            .path()
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "x64pcache"))
            .count()
    };
    assert_eq!(artifacts(), 1, "nested developer tools published unauthorised cache artifacts");
    let warm = run("warm");
    assert!(
        warm.contains("[pcache] HIT (translation skipped)"),
        "warm run did not reuse translated code: {warm}"
    );
    assert_eq!(artifacts(), 1, "warm nested execs expanded the authenticated cache surface");
}
