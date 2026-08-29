//! Ignored product-path characterization for persistent translation-cache reuse.

use hl_container::{Config, ContainerSpec, Containers, Execution, ExitStatus, Guest, Isolation, Process, Sandbox};
use hl_images::{Images, Platform, Reference, RuntimeConfig};
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt as _, path::Path, time::Instant};

type Error = Box<dyn std::error::Error>;

const UNIT_127_ASSEMBLY: &str = "a1d41926570d6ddfee050116a5698a9e5f7d2b7accf0dcfce685e46c707a7265";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Interpreter,
    Translated,
    CacheCold,
    CacheValid,
    CacheBitflip,
    CacheTruncated,
    ForkNoExec,
    ForkExec,
}

impl Mode {
    fn read() -> Result<Self, Error> {
        match std::env::var("HL_PCACHE_PROFILE_MODE")?.as_str() {
            "interpreter" => Ok(Self::Interpreter),
            "translated" => Ok(Self::Translated),
            "cache-cold" => Ok(Self::CacheCold),
            "cache-valid" => Ok(Self::CacheValid),
            "cache-bitflip" => Ok(Self::CacheBitflip),
            "cache-truncated" => Ok(Self::CacheTruncated),
            "fork-no-exec" => Ok(Self::ForkNoExec),
            "fork-exec" => Ok(Self::ForkExec),
            value => Err(format!("unknown HL_PCACHE_PROFILE_MODE {value:?}").into()),
        }
    }

    const fn cached(self) -> bool {
        !matches!(self, Self::Interpreter | Self::Translated)
    }

    const fn translated(self) -> bool {
        !matches!(self, Self::Interpreter)
    }
}

#[tokio::test]
#[ignore = "profile only: the runner supplies an owned compiler fixture and one isolated process arm"]
async fn compiler_process_reuses_the_product_translation_cache() -> Result<(), Error> {
    let mode = Mode::read()?;
    let selected = std::env::var("HL_TRANSLIT").unwrap_or_default();
    require(
        selected == if mode.translated() { "1" } else { "0" },
        "HL_TRANSLIT does not select the requested isolated-process arm",
    )?;
    let source = std::env::var_os("HL_PCACHE_PROFILE_ROOT")
        .ok_or("HL_PCACHE_PROFILE_ROOT must name the exact compiler fixture")?;
    let source = Path::new(&source);
    require(source.is_absolute(), "profile root is not absolute")?;
    require(
        source.join("work/src/unit_127.c").is_file(),
        "profile root has no unit_127 compiler input",
    )?;
    let cache = std::env::var_os("HL_PCACHE_PROFILE_CACHE")
        .ok_or("HL_PCACHE_PROFILE_CACHE must name the runner-owned persistent directory")?;
    let cache = Path::new(&cache);
    require(cache.is_absolute(), "profile cache is not absolute")?;
    if matches!(mode, Mode::CacheCold | Mode::ForkNoExec | Mode::ForkExec) {
        require(
            !cache.exists() || cache.read_dir()?.next().is_none(),
            "cold arm did not begin with an empty cache",
        )?;
    } else if matches!(mode, Mode::CacheValid | Mode::CacheBitflip | Mode::CacheTruncated) {
        require(
            cache.read_dir()?.next().is_some(),
            "warm arm began without a published cache",
        )?;
    }
    let owned = tempfile::tempdir()?;
    let images = Images::open(owned.path().join("images"))?;
    let layer = owned.path().join("fixture.tar");
    let archive = fs::File::create(&layer)?;
    let mut archive = tar::Builder::new(archive);
    archive.follow_symlinks(false);
    archive.append_dir_all(".", source).map_err(|error| {
        format!(
            "archive fixture {} without following symlinks: {error}",
            source.display()
        )
    })?;
    archive.finish()?;
    drop(archive);
    let input = fs::read(source.join("work/src/unit_127.c"))?;
    eprintln!("pcache-profile input_sha256={}", hex(&Sha256::digest(input)));

    let platform = Platform::linux_amd64();
    let name: Reference = "husklet.invalid/pcache-profile:fixture".parse()?;
    let image = images.import(
        fs::File::open(&layer)?,
        &RuntimeConfig {
            entrypoint: Vec::new(),
            command: Vec::new(),
            environment: BTreeMap::new(),
            working_directory: "/work".into(),
            user: String::new(),
        },
        &platform,
        &name,
    )?;
    eprintln!("pcache-profile image_digest={}", image.target.digest());
    let unpacked = images.unpack(&image, &platform)?;

    let config = Config::new(owned.path().join("state"));
    let config = if mode.cached() {
        config.translation_cache(cache)
    } else {
        config
    };
    let containers = Containers::builder(config).images(images.clone()).build().await?;
    if mode.cached() {
        let metadata = fs::symlink_metadata(cache)?;
        require(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "cache is not a real directory",
        )?;
        require(
            metadata.permissions().mode() & 0o777 == 0o700,
            "cache directory is not private",
        )?;
    }

    let root = images.roots().fork_overlay(unpacked.snapshot())?;
    // Launch cc1 itself: a shell parent forks before exit, and the production cache deliberately refuses to
    // publish a fork-inherited arena. This is the real compiler process whose reuse the fixture characterizes.
    let process = if mode == Mode::ForkNoExec {
        Process::new("/bin/sh").args(["-c", "(i=0; while [ $i -lt 10000 ]; do i=$((i+1)); done) & wait"])
    } else if mode == Mode::ForkExec {
        Process::new("/bin/sh").args(["-c", "/bin/true & wait"])
    } else {
        Process::new("/usr/libexec/gcc/x86_64-alpine-linux-musl/15.2.0/cc1").args([
            "-quiet", "/work/src/unit_127.c", "-quiet", "-dumpdir", "/tmp/", "-dumpbase", "unit_127.c",
            "-dumpbase-ext", ".c", "-mtune=generic", "-march=x86-64", "-g", "-O2", "-o", "-",
        ])
    };
    let spec = ContainerSpec::new(root, process)
        .name("pcache-profile")
        .guest(Guest::X86_64)
        .execution(if mode == Mode::Interpreter {
            Execution::Interpreted
        } else {
            Execution::native(false)
        })
        .isolation(Isolation {
            sandbox: Sandbox::Disabled,
            read_only_root: false,
            network_isolated: true,
            seccomp_baseline: hl_container::SeccompBaseline::Container,
        });
    containers.create(spec).await?;
    let started = Instant::now();
    containers.start("pcache-profile").await?;
    let status = containers.wait("pcache-profile").await?;
    let elapsed = started.elapsed();
    let logs = containers.logs("pcache-profile").await?;
    containers.remove("pcache-profile").await?;
    if status != ExitStatus::Code(0) {
        return Err(format!(
            "compiler workload exited {status:?}; stderr:\n{}",
            String::from_utf8_lossy(&logs.stderr)
        )
        .into());
    }
    let output: [u8; 32] = Sha256::digest(&logs.stdout).into();
    if matches!(mode, Mode::ForkNoExec | Mode::ForkExec) {
        require(logs.stdout.is_empty(), "fork lifecycle fixture produced output")?;
    } else {
        require(hex(&output) == UNIT_127_ASSEMBLY, "compiler workload output changed")?;
    }
    if mode.cached() {
        let entries = cache.read_dir()?.collect::<Result<Vec<_>, _>>()?;
        require(!entries.is_empty(), "cache arm published no entries")?;
        require(
            entries.iter().all(|entry| {
                entry.file_type().is_ok_and(|kind| kind.is_file() && !kind.is_symlink())
                    && entry
                        .metadata()
                        .is_ok_and(|metadata| metadata.permissions().mode() & 0o077 == 0)
            }),
            "cache contains a non-private or non-regular entry",
        )?;
        match mode {
            Mode::CacheCold => {}
            Mode::CacheValid => require(
                entries.iter().any(|entry| entry.file_name().as_encoded_bytes().ends_with(b".valid")),
                "valid same-ISA artifact did not reach the C validator's authenticated path",
            )?,
            Mode::CacheBitflip => require(
                entries.iter().any(|entry| entry.file_name().as_encoded_bytes().ends_with(b".checksum-invalid")),
                "bit-flipped artifact did not reach the checksum refusal path",
            )?,
            Mode::CacheTruncated => require(
                entries.iter().any(|entry| entry.file_name().as_encoded_bytes().ends_with(b".length-invalid")),
                "truncated artifact did not reach the structural-length refusal path",
            )?,
            Mode::ForkNoExec => require(
                entries.iter().any(|entry| entry.file_name().as_encoded_bytes().windows(14).any(|part| part == b".fork-refused-")),
                "fork child without exec did not refuse inherited cache publication",
            )?,
            Mode::ForkExec => require(
                entries.iter().filter(|entry| entry.file_name().as_encoded_bytes().windows(11).any(|part| part == b".published-")).count() >= 2,
                "fork+exec did not publish both parent and re-keyed child identities",
            )?,
            Mode::Interpreter | Mode::Translated => unreachable!(),
        }
    }
    eprintln!(
        "pcache-profile mode={} elapsed_us={} cache_loaded={} output={}",
        std::env::var("HL_PCACHE_PROFILE_MODE")?,
        elapsed.as_micros(),
        0,
        hex(&output)
    );
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn require(condition: bool, message: &'static str) -> Result<(), Error> {
    if condition { Ok(()) } else { Err(message.into()) }
}
