use std::fs::File;
use std::hint::black_box;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use hl_runtime::{GuestPathBytes, MountNamespace, ResolveRequest, Resolver};

use super::{pin::Host, source::MountPaths};

const ITERATIONS: usize = 25_000;
const WARMUPS: usize = 256;
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("hl_overlay_bench_{}_{}", std::process::id(), sequence));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn directory(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[derive(Clone, Copy)]
struct Sample {
    elapsed: Duration,
    iterations: usize,
}

impl Sample {
    fn nanoseconds(self) -> f64 {
        self.elapsed.as_nanos() as f64 / self.iterations as f64
    }
}

fn host(root: &Path) -> Host {
    Host::new(Arc::new(File::open(root).unwrap()), Arc::new(MountPaths::default()))
}

fn layered(upper: &Path, lower: &Path) -> Host {
    Host::layered(
        vec![
            Arc::new(File::open(upper).unwrap()),
            Arc::new(File::open(lower).unwrap()),
        ],
        Arc::new(MountPaths::default()),
    )
}

fn resolve_once(host: &Host, mounts: &MountNamespace, path: &GuestPathBytes, expected: bool) {
    let root = GuestPathBytes::new(b"/").unwrap();
    let resolver = Resolver::new(host.clone(), mounts);
    let result = resolver.resolve(ResolveRequest {
        path,
        base: &root,
        nofollow_final: true,
        no_symlinks: false,
        allow_missing_final: false,
    });
    match result {
        Ok(resolved) => {
            let lease = resolved.duplicate_parent().unwrap();
            let name = resolved.final_name().map_or_else(
                || c".".to_owned(),
                |name| std::ffi::CString::new(name.as_bytes()).unwrap(),
            );
            let path = Host::path(&lease, &name);
            assert_eq!(path.is_ok(), expected);
            black_box((lease.selected().as_raw_fd(), path.is_ok()));
        }
        Err(error) => {
            assert!(!expected);
            black_box(error);
        }
    }
}

fn measure(host: &Host, path: &[u8], expected: bool) -> Sample {
    let mounts = MountNamespace::new();
    let path = GuestPathBytes::new(path).unwrap();
    resolve_once(host, &mounts, &path, expected);
    for _ in 0..WARMUPS {
        resolve_once(host, &mounts, &path, expected);
    }
    let started = Instant::now();
    for _ in 0..ITERATIONS {
        resolve_once(host, &mounts, &path, expected);
    }
    Sample {
        elapsed: started.elapsed(),
        iterations: ITERATIONS,
    }
}

fn report(name: &str, sample: Sample, baseline: Sample) {
    eprintln!(
        "overlay_lookup {name}: {:.1} ns/op ({:.2}x ordinary raw-fd)",
        sample.nanoseconds(),
        sample.nanoseconds() / baseline.nanoseconds(),
    );
}

/// Release-only microbenchmark. It is deliberately ignored by normal tests.
///
/// Run after the overlay domain is released with:
/// `cargo test --release -p hl-engine overlay_resolution_microbench -- --ignored --nocapture`
#[test]
#[ignore = "release-only overlay performance evidence"]
fn overlay_resolution_microbench() {
    assert!(
        !cfg!(debug_assertions),
        "overlay performance evidence requires --release"
    );
    let fixture = Fixture::new();
    let ordinary = fixture.directory("ordinary");
    let upper = fixture.directory("upper");
    let lower = fixture.directory("lower");
    std::fs::write(ordinary.join("hit"), b"ordinary").unwrap();
    std::fs::write(upper.join("hit"), b"upper").unwrap();
    std::fs::write(lower.join("lower"), b"lower").unwrap();
    std::fs::write(lower.join("hidden"), b"hidden").unwrap();
    std::fs::write(upper.join(".wh.hidden"), b"").unwrap();

    let deep = "a/b/c/d/e/f/g/h/i/j/k/l";
    std::fs::create_dir_all(ordinary.join(deep)).unwrap();
    std::fs::create_dir_all(upper.join(deep)).unwrap();
    std::fs::create_dir_all(lower.join(deep)).unwrap();
    std::fs::write(ordinary.join(deep).join("leaf"), b"ordinary").unwrap();
    std::fs::write(lower.join(deep).join("leaf"), b"lower").unwrap();

    let ordinary_host = host(&ordinary);
    let ordinary_hit = measure(&ordinary_host, b"/hit", true);
    let ordinary_deep = measure(&ordinary_host, b"/a/b/c/d/e/f/g/h/i/j/k/l/leaf", true);
    report("ordinary_upper_control", ordinary_hit, ordinary_hit);
    report(
        "upper_hit",
        measure(&layered(&upper, &lower), b"/hit", true),
        ordinary_hit,
    );
    report(
        "lower_hit",
        measure(&layered(&upper, &lower), b"/lower", true),
        ordinary_hit,
    );
    report(
        "whiteout_miss",
        measure(&layered(&upper, &lower), b"/hidden", false),
        ordinary_hit,
    );
    report(
        "deep_lower_hit",
        measure(&layered(&upper, &lower), b"/a/b/c/d/e/f/g/h/i/j/k/l/leaf", true),
        ordinary_deep,
    );
    black_box(ordinary_host);
}
