# Husklet rebrand contract map

This map classifies rename surfaces by failure mode. The detailed historical occurrences are under
[`research/`](research/); the current-tree refresh must regenerate exact paths before implementation.

## 1. Build graph and Rust identity

All 18 current `dd-*` packages/directories rename to `husklet-*`: JIT (2), images, daemon, client, tests,
CLI, GUI, terminal core, GPU, display, compositor, wgpu backend, common shim, GL shim, CUDA driver shim,
CUDA runtime shim and Vulkan shim. Update together:

- root workspace `members` and `default-members`;
- each `[package].name`, explicit `[lib].name`, `[[bin]].name`, `default-run` and package-qualified command;
- every path dependency, `package =`, dependency alias, optional `dep:` feature and target dev-dependency;
- Rust imports (`dd_*`), doctests, examples, build scripts and generated Rust paths;
- Make, Nix, package scripts, release workflows, cache keys and artifact discovery globs;
- phase-2 crate-owned test invocations.

Cargo package and library identity may change without changing standard external ABIs. Do not rename
Khronos `egl/gl/vk*`, CUDA `cu*/cuda*`, Docker API names or Wayland protocol symbols.

## 2. Executables and shipped libraries

| Current artifact | Proposed artifact | Contract consumers |
|---|---|---|
| `ddcli` | `husklet` | shell users, GUI install/launch, package resources, docs, updater |
| `dd-daemon` | `husklet-daemon` | CLI/GUI discovery, launchd, scenarios, package bundle |
| `dd-app` | `husklet-app` | app bundle executable, updater/reopen, screenshot tooling |
| `dd-term` | `husklet-term` | manual launch, possible bundle decision |
| `dd-display` | `husklet-display` | CLI launcher, compositor fallback, test tooling, package resources |
| `dd-compositor` | `husklet-compositor` | display selector/exec fallback, mac gate, package resources |
| `dd-tests` | helper package only; no product runner name | phase 2 removes aggregate runner ownership |
| `ddjit-linux_aarch64`, `ddjit-linux_x86_64`, `ddjit-darwin_aarch64` | `hljit-*` or `husklet-jit-*`—decide once | build.rs output, `Guest::resolve_bundled`, packaging, diagnostics, tests |
| `darwinjail.dylib` | keep technical name unless product requires it | loader/package contract; not visibly `dd` |
| `libvk_dd.so(.1)` | `libvk_husklet.so(.1)` | ICD JSON, SONAME, loader/smoke tests, packaging |
| `libEGL`, `libGLESv2`, `libcuda`, `libcudart` | keep standard sonames | unmodified guest applications require them |

Do not rename executable paths until all resolvers use constants or a generated artifact manifest. Avoid a
temporary “try both names” fallback unless cross-version compatibility is explicitly selected; it can hide
an incomplete bundle.

## 3. Environment contracts

### Cross-process launch group

These must change setter and reader in one wave: `DDOCKERD_SOCK`, `DD_IMAGES`, `DD_STATE`, daemon
`DD_VOLUMES`, `DDJIT_DIR`, checkpoint/restore paths, display/GPU socket overrides, `DD_GUEST_ENV`, typed
container controls (`DD_ROOTFS`, cwd, uid/gid, hostname, limits, network, mounts/publish), GPU injection and
guest shim variables. A half rename silently selects defaults and can launch the wrong daemon, engine,
state store or socket.

Use semantic targets, not blind prefix substitution:

- `DDOCKERD_SOCK` → `HL_DAEMON_SOCK`;
- daemon `DD_VOLUMES` → `HL_VOLUMES_DIR`;
- engine volume list (`DDVOL`/old engine meaning) → `HL_MOUNTS` or another typed name;
- `DDJIT_DIR` → `HL_JIT_DIR`, retaining the subsystem name rather than ambiguous `HL_DIR`;
- `DDJIT_CHECKPOINT_DIR`/`RESTORE_DIR` → `HL_CHECKPOINT_DIR`/`HL_RESTORE_DIR`;
- `DD_GPU_EXEC` and host `DD_GPU_EXEC_SOCK` remain distinct guest endpoint vs host override names;
- `DD_DISPLAY_SOCK` remains a host display endpoint override.

### Diagnostic/test group

Display dumps, shader/texture/IR traces, golden-update flags, shim strict/debug, screenshot automation and
scenario selectors can mechanically become `HL_*`, but only retained controls are renamed. Phase 1 removes
orphaned terminal and JIT experiment flags first so Husklet does not canonize dead diagnostics.

### Bare JIT controls

`CRASHDBG`, `JT/JTS`, `PROF`, `NOSTITCH`, `NOSMC`, `NOFAST*`, flag-elision switches and other bare engine
variables are externally observable despite lacking `DD`. Rename only the supported allowlist to
`HL_JIT_*` or typed Rust configuration. Phase 1 must retire abandoned experiments and fix pcache identity
before this group. Do not map unrelated controls to one flat name.

### Standard environment exclusions

Keep `HOME`, `PATH`, `TMPDIR`, `SHELL`, `TERM`, `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR`, Vulkan loader standard
variables, Cargo build variables, compiler variables and application fixture variables.

## 4. FFI, C ABI and generated code

The Rust/C launch seam is one atomic unit: `ddjit_spawn`, configuration structs, magic/version constants,
headers, Rust declarations, C definitions, layout/offset/size assertions, generated bindings and any
dlsym/export lists. Rename to one `hl_*` family only after recording the existing ABI manifest.

Internal include guards and private macros may become `HL_*`. Linux/Khronos/CUDA constants must remain
standard. A macro beginning `DD_` is not necessarily a brand surface: local BPF/seccomp/futex helper
constants are private namespace choices and can be renamed mechanically with their file, but they carry no
cross-process compatibility and should not be mixed with the risky FFI wave.

Generated surfaces require changing the generator/input and regenerating output—never hand-edit generated
files. Export manifests must distinguish standard API exports, which stay unchanged, from project loader
entry/library names.

## 5. Runtime endpoints and services

| Contract | Current | Proposed | Atomic peers |
|---|---|---|---|
| state root | `~/.dd` | `~/.husklet` | CLI, GUI, daemon, tests, Make, package, updater |
| Docker socket | `~/.dd/run/docker.sock` | `~/.husklet/run/docker.sock` | keep basename for Docker compatibility |
| Wayland socket host path | under old run root | under Husklet run root | launcher, display/compositor |
| GPU host socket | `dd-gpu.sock` | `husklet-gpu.sock` | launcher, both compositor paths |
| guest GPU endpoint | `/run/user/0/dd-gpu-0` | `/run/user/0/husklet-gpu-0` | device mount + all shims/fixtures |
| Mach GPU service | `com.dd.display.gpu` | `com.husklet.display.gpu` | engine C, display Rust, bridge clients/tests |
| app IDs | `com.dd.app`, `.term` | `com.husklet.app`, `.term` | GTK/AppKit activation, Info.plist |
| daemon launchd label | `com.dd.daemon` | `com.husklet.daemon` | install/bootout/status/uninstall plist |
| application bundle | `/Applications/dd.app` | `/Applications/husklet.app` | install, update, discovery, release assets |
| logs | `~/Library/Logs/dd` | `~/Library/Logs/husklet` | launcher, snapshot, uninstall |

Temporary diagnostic/test filenames can change in their owning test patch but are not compatibility
contracts. Do not spend a risky atomic wave renaming every `/tmp/dd_*` fixture string.

## 6. Persisted and external formats

State JSON field names that do not contain the brand remain unchanged. Brand-bearing filenames and keys
need explicit policy:

- workspace/terminal config and all state/cache/image roots move with `~/.husklet`;
- `dd-image.json`, `dd-manifest.json`, alias directories and build-cache metadata require versioned
  read/write policy; filename replacement without dual-read or explicit rejection loses images/archives;
- `user.dd.*`/`user.ddx.*` xattrs require a migration or fresh-cutover rejection path;
- pcache/checkpoint files embed engine/config identity; brand-only changes still require cache invalidation
  if paths, ABI or configuration hashes change;
- Docker context `dd`, system info strings and runtime labels become Husklet user-facing values;
- remote image references change only after the new image exists and is pinned/tested.

The selected fresh cutover means the new process must not silently read and mutate `~/.dd`. It should
detect it and emit a precise migration/purge instruction, or run an explicitly invoked migration tool.

## 7. User-visible content and repository metadata

Rename package descriptions, CLI help, log prefixes, app titles, website copy/media names, README/docs,
release artifact names, updater URLs and GitHub workflow labels. Preserve historical Git commits and
third-party reference content. Rename media files and their HTML/CSS references together; validate links
and rendered pages, not just text matches.
