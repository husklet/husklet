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
    CacheFreshRollover,
    CacheSemanticMap,
    CacheSemanticDuplicate,
    CacheSemanticHelper,
    CacheSemanticRelocation,
    CacheSemanticLibrary,
    CacheSemanticOverlap,
    CacheSemanticChain,
    CacheChangedLibrary,
    CacheAbsentLibrary,
    CacheStageFailure,
    CacheThreadCold,
    CacheThreadValid,
    CacheValid,
    CacheAuthorityReuse,
    CacheBitflip,
    CacheTruncated,
    ForkNoExec,
    ForkExec,
    RelocationMissing,
}

impl Mode {
    fn read() -> Result<Self, Error> {
        match std::env::var("HL_PCACHE_PROFILE_MODE")?.as_str() {
            "interpreter" => Ok(Self::Interpreter),
            "translated" => Ok(Self::Translated),
            "cache-cold" => Ok(Self::CacheCold),
            "cache-fresh-rollover" => Ok(Self::CacheFreshRollover),
            "cache-semantic-map" => Ok(Self::CacheSemanticMap),
            "cache-semantic-duplicate" => Ok(Self::CacheSemanticDuplicate),
            "cache-semantic-helper" => Ok(Self::CacheSemanticHelper),
            "cache-semantic-relocation" => Ok(Self::CacheSemanticRelocation),
            "cache-semantic-library" => Ok(Self::CacheSemanticLibrary),
            "cache-semantic-overlap" => Ok(Self::CacheSemanticOverlap),
            "cache-semantic-chain" => Ok(Self::CacheSemanticChain),
            "cache-changed-library" => Ok(Self::CacheChangedLibrary),
            "cache-absent-library" => Ok(Self::CacheAbsentLibrary),
            "cache-stage-failure" => Ok(Self::CacheStageFailure),
            "cache-thread-cold" => Ok(Self::CacheThreadCold),
            "cache-thread-valid" => Ok(Self::CacheThreadValid),
            "cache-valid" => Ok(Self::CacheValid),
            "cache-authority-reuse" => Ok(Self::CacheAuthorityReuse),
            "cache-bitflip" => Ok(Self::CacheBitflip),
            "cache-truncated" => Ok(Self::CacheTruncated),
            "fork-no-exec" => Ok(Self::ForkNoExec),
            "fork-exec" => Ok(Self::ForkExec),
            "relocation-missing" => Ok(Self::RelocationMissing),
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
    if matches!(
        mode,
        Mode::CacheCold | Mode::CacheAuthorityReuse | Mode::CacheThreadCold | Mode::CacheFreshRollover |
            Mode::ForkNoExec | Mode::ForkExec | Mode::RelocationMissing
    ) {
        require(
            !cache.exists() || cache.read_dir()?.next().is_none(),
            "cold arm did not begin with an empty cache",
        )?;
    } else if matches!(
        mode,
        Mode::CacheValid
            | Mode::CacheBitflip
            | Mode::CacheTruncated
            | Mode::CacheSemanticMap
            | Mode::CacheSemanticDuplicate
            | Mode::CacheSemanticHelper
            | Mode::CacheSemanticRelocation
            | Mode::CacheSemanticLibrary
            | Mode::CacheSemanticOverlap
            | Mode::CacheSemanticChain
            | Mode::CacheChangedLibrary
            | Mode::CacheAbsentLibrary
            | Mode::CacheStageFailure
            | Mode::CacheThreadValid
    ) {
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
    if mode == Mode::CacheChangedLibrary {
        // A trailing byte leaves the ELF load image and compiler behavior unchanged while changing the
        // complete-content authority. Append the replacement last so the imported image contains the
        // changed library rather than merely corrupting the persisted manifest.
        let relative = "usr/lib/libisl.so.23.3.0";
        let mut changed = fs::read(source.join(relative))?;
        changed.push(0);
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o755);
        header.set_size(changed.len() as u64);
        header.set_cksum();
        archive.append_data(&mut header, relative, changed.as_slice())?;
    }
    if matches!(mode, Mode::CacheThreadCold | Mode::CacheThreadValid) {
        const SOURCE: &[u8] = br#"#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <sys/mman.h>
#include <unistd.h>
static int stop;
static void *peer(void *unused) { (void)unused; while (!__atomic_load_n(&stop, __ATOMIC_ACQUIRE)) {} return 0; }
extern int warm_source(int), warm_target(void);
__asm__(".text\n.p2align 4\n.global warm_source,warm_target\n"
        "warm_source: test %rdi,%rdi; jne warm_target; xor %eax,%eax; ret\n"
        "warm_target: mov $1,%eax; ret\n");
int main(void) {
  if (warm_target() != 1 || warm_source(1) != 1) return 1;
  pthread_t thread;
  if (pthread_create(&thread, 0, peer, 0)) return 2;
  int fd = open("/work/executable-page.bin", O_RDONLY);
  if (fd < 0) return 3;
  void *mapping = mmap(0, 4096, PROT_READ | PROT_EXEC, MAP_PRIVATE, fd, 0);
  if (mapping == MAP_FAILED) return 4;
  __atomic_store_n(&stop, 1, __ATOMIC_RELEASE);
  if (pthread_join(thread, 0)) return 5;
  puts("thread-warm-ok");
  return 0;
}
"#;
        let host_source = owned.path().join("thread_warm.c");
        let host_binary = owned.path().join("thread_warm");
        fs::write(&host_source, SOURCE)?;
        let built = std::process::Command::new("cc")
            .args(["-O2", "-pthread", "-Wl,--build-id=none", "-o"])
            .arg(&host_binary)
            .arg(&host_source)
            .status()?;
        require(built.success(), "pinned host compiler did not build the static pthread fixture")?;
        let binary = fs::read(&host_binary)?;
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o755);
        header.set_size(binary.len() as u64);
        header.set_cksum();
        archive.append_data(&mut header, "work/thread_warm", binary.as_slice())?;
        let linked = std::process::Command::new("ldd").arg(&host_binary).output()?;
        require(linked.status.success(), "host linker census failed for pthread fixture")?;
        let mut dependencies = String::from_utf8(linked.stdout)?
            .split_whitespace()
            .filter(|field| field.starts_with('/') && Path::new(field).is_file())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        dependencies.sort();
        dependencies.dedup();
        require(!dependencies.is_empty(), "pthread fixture reported no dynamic dependencies")?;
        for dependency in dependencies {
            let bytes = fs::read(&dependency)?;
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o755);
            header.set_size(bytes.len() as u64);
            header.set_cksum();
            archive.append_data(&mut header, dependency.trim_start_matches('/'), bytes.as_slice())?;
        }
        let page = [0u8; 4096];
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(page.len() as u64);
        header.set_cksum();
        archive.append_data(&mut header, "work/executable-page.bin", page.as_slice())?;
    }
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
    let config = config.translation_cache_observability(
        std::env::var("HL_PCACHE_PROFILE_OBSERVE").is_ok_and(|value| value == "1"),
    );
    let config = match std::env::var_os("HL_PCACHE_PROFILE_SYMBOLS") {
        Some(directory) => config.translation_symbols(directory),
        None => config,
    };
    let containers = Containers::builder(config).images(images.clone()).build().await?;
    if mode == Mode::RelocationMissing {
        require(
            std::env::var_os("HL_TRANSLIT_PCACHE_DROP_RELOCATION_TEST").is_some(),
            "relocation mutation arm did not enable its native hook",
        )?;
    }
    if mode == Mode::CacheFreshRollover {
        require(
            std::env::var_os("HL_TRANSLIT_PERF_FRESH_ROLLOVER_TEST").is_some(),
            "fresh-rollover arm did not enable its native hook",
        )?;
    }
    if mode == Mode::CacheStageFailure {
        require(
            std::env::var_os("HL_TRANSLIT_PCACHE_SINGLE_MAP_TEST").is_some(),
            "warm failure stage did not force the single-map W^X arena",
        )?;
        let stage = std::env::var("HL_TRANSLIT_PCACHE_WARM_FAIL_STAGE")?;
        require(
            [
                "arena-copy",
                "relocation",
                "map-build",
                "owner-build",
                "source-build",
                "chain-fallback",
                "manifest-activation",
            ]
            .contains(&stage.as_str()),
            "unknown warm failure stage",
        )?;
    }
    if matches!(
        mode,
        Mode::CacheSemanticMap
            | Mode::CacheSemanticDuplicate
            | Mode::CacheSemanticHelper
            | Mode::CacheSemanticRelocation
            | Mode::CacheSemanticLibrary
            | Mode::CacheSemanticOverlap
            | Mode::CacheSemanticChain
            | Mode::CacheAbsentLibrary
    ) {
        let artifact = cache
            .read_dir()?
            .find_map(|entry| {
                let entry = entry.ok()?;
                entry
                    .file_name()
                    .as_encoded_bytes()
                    .ends_with(b".x64pcache")
                    .then_some(entry.path())
            })
            .ok_or("semantic corruption arm found no cache artifact")?;
        let mut bytes = fs::read(&artifact)?;
        require(bytes.len() >= 256, "semantic corruption artifact is truncated")?;
        let get = |offset| u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        let maps = get(96) as usize;
        let owners = get(104) as usize;
        let relocations = get(184) as usize;
        let libraries = get(232) as usize;
        let chains = get(240) as usize;
        let arena = get(88);
        match mode {
            Mode::CacheSemanticMap => {
                require(maps != 0, "semantic map corruption has no map record")?;
                bytes[256 + 8..256 + 16].copy_from_slice(&u64::MAX.to_le_bytes());
            }
            Mode::CacheSemanticDuplicate => {
                require(
                    maps >= 2,
                    "semantic duplicate corruption has fewer than two map records",
                )?;
                let first = bytes[256..256 + 84].to_vec();
                bytes[256 + 84..256 + 168].copy_from_slice(&first);
            }
            Mode::CacheSemanticHelper => bytes[120..128].copy_from_slice(&arena.to_le_bytes()),
            Mode::CacheSemanticRelocation => {
                require(
                    relocations != 0,
                    "semantic relocation corruption has no relocation record",
                )?;
                let offset = 256 + maps * 84 + owners * 24 + 4;
                bytes[offset..offset + 4].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
            }
            Mode::CacheSemanticLibrary => {
                require(libraries != 0, "semantic library corruption has no manifest record")?;
                let offset = 256 + maps * 84 + owners * 24 + relocations * 8;
                bytes[offset..offset + 8].copy_from_slice(&u64::MAX.to_le_bytes());
            }
            Mode::CacheSemanticOverlap => {
                require(
                    libraries >= 2,
                    "semantic overlap corruption has fewer than two libraries",
                )?;
                let libraries_at = 256 + maps * 84 + owners * 24 + relocations * 8;
                let first_base = get(libraries_at);
                let first_len = get(libraries_at + 8);
                require(first_len > 1, "first manifest span is too short to overlap")?;
                let second = libraries_at + 56;
                bytes[second..second + 8].copy_from_slice(&(first_base + first_len - 1).to_le_bytes());
            }
            Mode::CacheSemanticChain => {
                require(chains != 0, "semantic chain corruption has no chain record")?;
                let offset = 256 + maps * 84 + owners * 24 + relocations * 8 + libraries * 56 + 4;
                bytes[offset..offset + 4].copy_from_slice(&(arena as u32).to_le_bytes());
            }
            Mode::CacheAbsentLibrary => {
                require(libraries != 0, "absent-library arm has no manifest record")?;
                let maps_at = 256;
                let owners_at = maps_at + maps * 84;
                let relocations_at = owners_at + owners * 24;
                let libraries_at = relocations_at + relocations * 8;
                let chains_at = libraries_at + libraries * 56;
                let arena_at = chains_at + chains * 24;
                let selected = (0..libraries)
                    .find(|library| {
                        let at = libraries_at + library * 56;
                        let base = get(at);
                        let end = base.saturating_add(get(at + 8));
                        bytes[maps_at..owners_at].chunks_exact(84).any(|record| {
                            let start = u64::from_le_bytes(record[8..16].try_into().unwrap());
                            let finish = u64::from_le_bytes(record[16..24].try_into().unwrap());
                            start >= base && finish <= end
                        })
                    })
                    .ok_or("absent-library arm found no manifest-owned block")?;
                let selected_at = libraries_at + selected * 56;
                let lib_base = get(selected_at);
                let lib_len = get(selected_at + 8);
                let lib_end = lib_base.checked_add(lib_len).ok_or("library span overflow")?;
                let mut kept_maps = Vec::new();
                let mut removed_maps = 0usize;
                for record in bytes[maps_at..owners_at].chunks_exact(84) {
                    let start = u64::from_le_bytes(record[8..16].try_into().unwrap());
                    let end = u64::from_le_bytes(record[16..24].try_into().unwrap());
                    if start >= lib_base && end <= lib_end {
                        removed_maps += 1;
                    } else {
                        kept_maps.extend_from_slice(record);
                    }
                }
                require(removed_maps != 0, "selected absent library owned no cached blocks")?;
                let mut kept_chains = Vec::new();
                for record in bytes[chains_at..arena_at].chunks_exact(24) {
                    let source = u64::from_le_bytes(record[8..16].try_into().unwrap());
                    let target = u64::from_le_bytes(record[16..24].try_into().unwrap());
                    if !((source >= lib_base && source < lib_end) || (target >= lib_base && target < lib_end)) {
                        kept_chains.extend_from_slice(record);
                    }
                }
                let mut rebuilt = bytes[..256].to_vec();
                rebuilt[96..104].copy_from_slice(&(maps - removed_maps).to_le_bytes());
                rebuilt[232..240].copy_from_slice(&(libraries - 1).to_le_bytes());
                rebuilt[240..248].copy_from_slice(&(kept_chains.len() / 24).to_le_bytes());
                rebuilt.extend_from_slice(&kept_maps);
                rebuilt.extend_from_slice(&bytes[owners_at..libraries_at]);
                rebuilt.extend_from_slice(&bytes[libraries_at..selected_at]);
                rebuilt.extend_from_slice(&bytes[selected_at + 56..chains_at]);
                rebuilt.extend_from_slice(&kept_chains);
                rebuilt.extend_from_slice(&bytes[arena_at..]);
                bytes = rebuilt;
            }
            _ => unreachable!(),
        }
        bytes[248..256].fill(0);
        let mut digest = 1_469_598_103_934_665_603u64;
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            digest ^= u64::from_le_bytes(chunk.try_into().unwrap());
            digest = digest.wrapping_mul(1_099_511_628_211);
        }
        for byte in chunks.remainder() {
            digest ^= u64::from(*byte);
            digest = digest.wrapping_mul(1_099_511_628_211);
        }
        bytes[248..256].copy_from_slice(&digest.to_le_bytes());
        fs::write(artifact, bytes)?;
    }
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
    } else if matches!(mode, Mode::CacheThreadCold | Mode::CacheThreadValid) {
        Process::new("/work/thread_warm")
    } else {
        Process::new("/usr/libexec/gcc/x86_64-alpine-linux-musl/15.2.0/cc1").args([
            "-quiet",
            "/work/src/unit_127.c",
            "-quiet",
            "-dumpdir",
            "/tmp/",
            "-dumpbase",
            "unit_127.c",
            "-dumpbase-ext",
            ".c",
            "-mtune=generic",
            "-march=x86-64",
            "-g",
            "-O2",
            "-o",
            "-",
        ])
    };
    let repeat_process = process.clone();
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
    benchmark_barrier("ready", "release")?;
    let started = Instant::now();
    containers.start("pcache-profile").await?;
    let status = containers.wait("pcache-profile").await?;
    let elapsed = started.elapsed();
    let logs = containers.logs("pcache-profile").await?;
    containers.remove("pcache-profile").await?;
    if mode == Mode::CacheAuthorityReuse {
        let repeat_root = images.roots().fork_overlay(unpacked.snapshot())?;
        let repeat = ContainerSpec::new(repeat_root, repeat_process)
            .name("pcache-profile-repeat")
            .guest(Guest::X86_64)
            .execution(Execution::native(false))
            .isolation(Isolation {
                sandbox: Sandbox::Disabled,
                read_only_root: false,
                network_isolated: true,
                seccomp_baseline: hl_container::SeccompBaseline::Container,
            });
        containers.create(repeat).await?;
        containers.start("pcache-profile-repeat").await?;
        let repeat_status = containers.wait("pcache-profile-repeat").await?;
        let repeat_logs = containers.logs("pcache-profile-repeat").await?;
        containers.remove("pcache-profile-repeat").await?;
        require(repeat_status == ExitStatus::Code(0), "repeated compiler process failed")?;
        require(repeat_logs.stdout == logs.stdout, "repeated compiler output changed")?;
    }
    benchmark_barrier("done", "finish")?;
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
    } else if matches!(mode, Mode::CacheThreadCold | Mode::CacheThreadValid) {
        require(logs.stdout == b"thread-warm-ok\n", "threaded executable-map workload output changed")?;
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
            Mode::CacheCold | Mode::CacheAuthorityReuse | Mode::CacheThreadCold => require(
                entries
                    .iter()
                    .any(|entry| entry.file_name().as_encoded_bytes().ends_with(b".x64pcache"))
                    && (!cfg!(feature = "native-test-hooks") || entries.iter().any(|entry| {
                        entry
                            .file_name()
                            .as_encoded_bytes()
                            .windows(11)
                            .any(|part| part == b".published-")
                    })),
                "cold arm did not publish a cache artifact (and hook receipt when enabled)",
            )?,
            Mode::CacheFreshRollover => require(
                entries.iter().any(|entry| {
                    entry
                        .file_name()
                        .as_encoded_bytes()
                        .windows(27)
                        .any(|part| part == b".relocation-rollover-exact-")
                }),
                "fresh generation did not publish an exact relocation-ledger receipt",
            )?,
            Mode::CacheSemanticMap
            | Mode::CacheSemanticDuplicate
            | Mode::CacheSemanticHelper
            | Mode::CacheSemanticRelocation
            | Mode::CacheSemanticLibrary
            | Mode::CacheSemanticOverlap
            | Mode::CacheSemanticChain => require(
                entries
                    .iter()
                    .any(|entry| entry.file_name().as_encoded_bytes().ends_with(b".length-invalid"))
                    && !entries.iter().any(|entry| {
                        entry
                            .file_name()
                            .as_encoded_bytes()
                            .windows(5)
                            .any(|part| part == b".hit-")
                    }),
                "rechecksummed semantic corruption did not remain a pristine MISS",
            )?,
            Mode::CacheChangedLibrary => {
                require(
                    entries
                        .iter()
                        .any(|entry| entry.file_name().as_encoded_bytes().ends_with(b".valid"))
                        && entries.iter().any(|entry| {
                            entry
                                .file_name()
                                .as_encoded_bytes()
                                .windows(5)
                                .any(|part| part == b".hit-")
                        })
                        && entries.iter().any(|entry| {
                            entry
                                .file_name()
                                .as_encoded_bytes()
                                .windows(18)
                                .any(|part| part == b".library-mismatch-")
                        }),
                    "changed library did not authenticate structure and defer on content mismatch",
                )?;
                let stats = entries
                    .iter()
                    .find(|entry| {
                        entry
                            .file_name()
                            .as_encoded_bytes()
                            .windows(12)
                            .any(|part| part == b".warm-stats-")
                    })
                    .ok_or("changed library emitted no translation-count receipt")?;
                let bytes = fs::read(stats.path())?;
                require(bytes.len() == 24, "changed-library translation receipt has wrong size")?;
                let translated = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
                require(
                    translated != 0,
                    "changed library executed restored blocks instead of translating",
                )?;
            }
            Mode::CacheAbsentLibrary => {
                require(
                    entries
                        .iter()
                        .any(|entry| entry.file_name().as_encoded_bytes().ends_with(b".valid"))
                        && entries.iter().any(|entry| {
                            entry
                                .file_name()
                                .as_encoded_bytes()
                                .windows(5)
                                .any(|part| part == b".hit-")
                        })
                        && entries.iter().any(|entry| {
                            entry
                                .file_name()
                                .as_encoded_bytes()
                                .windows(16)
                                .any(|part| part == b".library-absent-")
                        }),
                    "absent library did not remain deferred and unpublished",
                )?;
                let stats = entries
                    .iter()
                    .find(|entry| {
                        entry
                            .file_name()
                            .as_encoded_bytes()
                            .windows(12)
                            .any(|part| part == b".warm-stats-")
                    })
                    .ok_or("absent library emitted no translation-count receipt")?;
                let bytes = fs::read(stats.path())?;
                require(
                    bytes.len() == 24 && u64::from_le_bytes(bytes[16..24].try_into().unwrap()) != 0,
                    "absent library did not force fresh translation",
                )?;
            }
            Mode::CacheStageFailure => {
                let stage = std::env::var("HL_TRANSLIT_PCACHE_WARM_FAIL_STAGE")?;
                let needle = format!(".stage-{stage}-rollback-");
                let receipt = entries
                    .iter()
                    .find(|entry| String::from_utf8_lossy(entry.file_name().as_encoded_bytes()).contains(&needle))
                    .ok_or("warm failure stage emitted no rollback receipt")?;
                let state = fs::read(receipt.path())?;
                require(state.len() == 40, "rollback receipt has wrong size")?;
                let word = |offset| u64::from_le_bytes(state[offset..offset + 8].try_into().unwrap());
                require(
                    word(0) == 1 && word(1) == 0 && word(2) == 0 && word(3) == 0 && word(4) == 0,
                    "warm failure did not leave a pristine empty executable single-map generation",
                )?;
            }
            Mode::CacheThreadValid => require(
                entries.iter().any(|entry| {
                    entry.file_name().as_encoded_bytes().windows(5).any(|part| part == b".hit-")
                }) && entries.iter().any(|entry| {
                    entry.file_name().as_encoded_bytes().windows(20).any(|part| part == b".thread-start-state-")
                        && fs::read(entry.path()).is_ok_and(|state| {
                            state.len() == 24 && u64::from_le_bytes(state[..8].try_into().unwrap()) == 1
                        })
                }) && entries.iter().any(|entry| {
                    entry.file_name().as_encoded_bytes().windows(18).any(|part| part == b".thread-map-state-")
                        && fs::read(entry.path()).is_ok_and(|state| state == [0u8; 16])
                }),
                "loaded warm authority survived real pthread start or remained patchable afterward",
            )?,
            // The production diagnostic is written by the native engine to the host diagnostic
            // descriptor, outside captured guest stderr; the external benchmark runner validates it.
            Mode::CacheValid if !cfg!(feature = "native-test-hooks") => {},
            Mode::CacheValid => require(
                entries.iter().any(|entry| entry.file_name().as_encoded_bytes().ends_with(b".valid")),
                "valid same-ISA artifact did not reach the C validator's authenticated path",
            )
            .and_then(|()| {
                require(
                    entries.iter().any(|entry| {
                        entry
                            .file_name()
                            .as_encoded_bytes()
                            .windows(5)
                            .any(|part| part == b".hit-")
                    }),
                    "authenticated cache was not restored as a warm HIT",
                )
            })
            .and_then(|()| {
                let stats = entries
                    .iter()
                    .find(|entry| {
                        entry
                            .file_name()
                            .as_encoded_bytes()
                            .windows(12)
                            .any(|part| part == b".warm-stats-")
                    })
                    .ok_or("warm HIT emitted no translation-count receipt")?;
                let bytes = fs::read(stats.path())?;
                require(bytes.len() == 24, "warm translation-count receipt has wrong size")?;
                let word = |offset| u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
                let restored = word(0);
                let translated = word(16);
                require(
                    restored != 0 && translated < restored / 2,
                    "warm HIT did not reduce translation count by at least one half",
                )
            })?,
            Mode::CacheBitflip => require(
                entries
                    .iter()
                    .any(|entry| entry.file_name().as_encoded_bytes().ends_with(b".checksum-invalid")),
                "bit-flipped artifact did not reach the checksum refusal path",
            )?,
            Mode::CacheTruncated => require(
                entries
                    .iter()
                    .any(|entry| entry.file_name().as_encoded_bytes().ends_with(b".length-invalid")),
                "truncated artifact did not reach the structural-length refusal path",
            )?,
            Mode::ForkNoExec => require(
                entries.iter().any(|entry| {
                    entry
                        .file_name()
                        .as_encoded_bytes()
                        .windows(14)
                        .any(|part| part == b".fork-refused-")
                }),
                "fork child without exec did not refuse inherited cache publication",
            )?,
            Mode::ForkExec => require(
                entries
                    .iter()
                    .filter(|entry| {
                        entry
                            .file_name()
                            .as_encoded_bytes()
                            .windows(11)
                            .any(|part| part == b".published-")
                    })
                    .count()
                    >= 2,
                "fork+exec did not publish both parent and re-keyed child identities",
            )?,
            Mode::RelocationMissing => require(
                entries.iter().any(|entry| {
                    entry
                        .file_name()
                        .as_encoded_bytes()
                        .windows(20)
                        .any(|part| part == b".relocation-refused-")
                }) && !entries.iter().any(|entry| {
                    entry
                        .file_name()
                        .as_encoded_bytes()
                        .windows(11)
                        .any(|part| part == b".published-")
                }),
                "unrecorded emitted absolute did not refuse cache publication",
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

/// Optional benchmark-only handshake. Image import, overlay creation, and process construction finish
/// before `ready`; the runner releases exactly one container start/wait/log/remove interval, stops its
/// counter at `done`, then acknowledges with `finish`. Ordinary tests set no directory and pay no cost.
fn benchmark_barrier(announce: &str, await_name: &str) -> Result<(), Error> {
    let Some(directory) = std::env::var_os("HL_PCACHE_PROFILE_BARRIER_DIR") else {
        return Ok(());
    };
    let directory = Path::new(&directory);
    require(directory.is_absolute(), "benchmark barrier directory is not absolute")?;
    require(directory.is_dir(), "benchmark barrier directory does not exist")?;
    fs::write(directory.join(announce), [])?;
    let awaited = directory.join(await_name);
    let deadline = Instant::now() + std::time::Duration::from_secs(120);
    while !awaited.is_file() {
        if Instant::now() >= deadline {
            return Err(format!("benchmark barrier timed out waiting for {}", awaited.display()).into());
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn require(condition: bool, message: &'static str) -> Result<(), Error> {
    if condition { Ok(()) } else { Err(message.into()) }
}
