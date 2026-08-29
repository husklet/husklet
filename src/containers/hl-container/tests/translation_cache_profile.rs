//! Ignored product-path characterization for persistent translation-cache reuse.

use hl_container::{Config, ContainerSpec, Containers, Execution, ExitStatus, Guest, Isolation, Process, Sandbox};
use hl_images::{Images, Platform, Reference, RuntimeConfig};
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt as _, path::Path, time::Instant};

type Error = Box<dyn std::error::Error>;

const UNIT_127_OBJECT: &str =
    "e1b634483ab1ed701be7f4004b3981d3e56c6d228763efe2496514b667c74f44  /tmp/unit_127.o\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Interpreter,
    Translated,
    CacheCold,
    CacheWarm,
}

impl Mode {
    fn read() -> Result<Self, Error> {
        match std::env::var("HL_PCACHE_PROFILE_MODE")?.as_str() {
            "interpreter" => Ok(Self::Interpreter),
            "translated" => Ok(Self::Translated),
            "cache-cold" => Ok(Self::CacheCold),
            "cache-warm" => Ok(Self::CacheWarm),
            value => Err(format!("unknown HL_PCACHE_PROFILE_MODE {value:?}").into()),
        }
    }

    const fn cached(self) -> bool {
        matches!(self, Self::CacheCold | Self::CacheWarm)
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
    if mode == Mode::CacheCold {
        require(
            !cache.exists() || cache.read_dir()?.next().is_none(),
            "cold arm did not begin with an empty cache",
        )?;
    } else if mode == Mode::CacheWarm {
        require(
            cache.read_dir()?.next().is_some(),
            "warm arm began without a published cache",
        )?;
    }
    let warm_receipt_before = warm_receipt_mtime(cache)?;

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
    let process = Process::new("/bin/sh").args([
        "-c",
        "/usr/libexec/gcc/x86_64-alpine-linux-musl/15.2.0/cc1 -quiet /work/src/unit_127.c -quiet -dumpdir /tmp/ -dumpbase unit_127.c -dumpbase-ext .c -mtune=generic -march=x86-64 -g -O2 -o /tmp/unit_127.s && /usr/bin/as --gdwarf-5 --64 -o /tmp/unit_127.o /tmp/unit_127.s && sha256sum /tmp/unit_127.o",
    ]);
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
    require(
        logs.stdout == UNIT_127_OBJECT.as_bytes(),
        "compiler workload did not produce the exact unit_127 object",
    )?;
    let warm_receipt_after = warm_receipt_mtime(cache)?;
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
            Mode::CacheCold => require(
                warm_receipt_after.is_none(),
                "cold publication unexpectedly produced a warm-load receipt",
            )?,
            Mode::CacheWarm => require(
                warm_receipt_after > warm_receipt_before,
                "independent warm process did not publish a fresh cache-load receipt",
            )?,
            Mode::Interpreter | Mode::Translated => unreachable!(),
        }
    }
    let output: [u8; 32] = Sha256::digest(&logs.stdout).into();
    eprintln!(
        "pcache-profile mode={} elapsed_us={} warm_hit={} output={}",
        std::env::var("HL_PCACHE_PROFILE_MODE")?,
        elapsed.as_micros(),
        usize::from(mode == Mode::CacheWarm && warm_receipt_after > warm_receipt_before),
        hex(&output)
    );
    Ok(())
}

fn warm_receipt_mtime(cache: &Path) -> Result<Option<std::time::SystemTime>, Error> {
    if !cache.exists() {
        return Ok(None);
    }
    let mut receipts = cache
        .read_dir()?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().as_encoded_bytes().ends_with(b".warm"))
        .map(|entry| entry.metadata().and_then(|metadata| metadata.modified()))
        .collect::<Result<Vec<_>, _>>()?;
    require(receipts.len() <= 1, "cache contains multiple warm-load receipts")?;
    Ok(receipts.pop())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn require(condition: bool, message: &'static str) -> Result<(), Error> {
    if condition { Ok(()) } else { Err(message.into()) }
}
