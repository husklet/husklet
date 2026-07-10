# Archive and Filesystem Compatibility Gaps

Date: 2026-07-10

These findings came from isolated workspaces `/tmp/dd-agent5-sparse.seZJ2E` and `/Users/x/dd/dd-verify-5b`. The main worktree was not modified.

## Docker Cp Put Follows Existing Destination Symlink

Priority: P1
Impact: silent wrong target for copied data
Confidence: High

Evidence:

- `docker cp` put extracts client tar with host `tar xf - -C <host>`: `dd-daemon/src/archive/handlers.rs:172`.

Why this is bad:

If the destination already contains `linkout -> ../outside` and the archive contains `linkout/file.txt`, GNU tar writes through the pre-existing symlink to the outside directory and exits `0`. For compatibility and data integrity, copy should land under the requested container path or fail cleanly.

Isolated proof:

PoC observed silent write to the symlink target outside the requested destination.

## `docker cp` GET Drops Lower Entries From Overlay Directories

Priority: P1
Impact: incomplete copied directories from containers
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-verify-5b`.

Evidence:

- `overlay_host_path` returns the upper path whenever it exists: `dd-daemon/src/archive/overlay.rs:79`.
- `archive_get` tars exactly that one physical path: `dd-daemon/src/archive/handlers.rs:62`, `dd-daemon/src/archive/handlers.rs:80`.

Why this is bad:

If an upper-layer file exists in a directory, `docker cp container:/dir -` can tar only the upper directory and omit lower-only entries. This likely affects paths like `/etc` once daemon-managed files exist in the upper layer.

Isolated proof:

```sh
cargo test -p dd-daemon poc_archive_get_directory_must_merge_upper_and_lower_entries -- --ignored
```

Observed: upper `/etc/hosts` caused `/etc` to resolve to upper `/etc`, so lower-only `/etc/alpine-release` was missing from the tar source.

## Anonymous Volume Copy-Up Drops Seeded Directory Metadata

Priority: P2
Impact: seeded volume contents diverge from image permissions and metadata
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-workerF-archive-audit-20260710`.

Evidence:

- Recursive copy-up creates directories with default mode: `dd-daemon/src/containers/lifecycle/create/volumes.rs:30`.
- File copy uses `std::fs::copy`, which does not preserve all metadata/xattrs/owners: `dd-daemon/src/containers/lifecycle/create/volumes.rs:40`.

Why this is bad:

Docker named/anonymous volume initialization copies existing image directory contents into the new volume. Dropping mode/metadata changes behavior for permissions-sensitive paths.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-workerF-target cargo test -p dd-daemon copy_dir_into_preserves_seeded_directory_mode -- --nocapture
```

Result: failed as expected; copied mode was `0755`, expected source mode `0711`.

## Dockerfile `COPY` / `ADD` Metadata Flags Are Ignored

Priority: P2
Impact: builds succeed while requested ownership/mode changes are absent
Confidence: High

Evidence:

- `copy_step` only recognizes `--from=`: `dd-daemon/src/build/steps.rs:184`.
- Other flags are filtered out before source/destination parsing: `dd-daemon/src/build/steps.rs:187`.
- Copy execution is plain `cp -a`: `dd-daemon/src/build/steps.rs:221`.

Why this is bad:

`COPY --chown=` and `COPY --chmod=` are common Dockerfile metadata controls. dd accepts and drops those flags, producing images with unexpected file ownership or mode.

Verification:

Add a Dockerfile build PoC with `COPY --chmod=0755 file /bin/tool` and inspect the resulting mode inside the image.

## Daemon Save/Load Drops Lifecycle Metadata

Priority: P2
Impact: saved images lose stop signal, volumes, and healthcheck metadata
Confidence: High

Evidence:

- Manifest supports `stop_signal`, `img_volumes`, and `healthcheck`: `dd-images/src/image/manifest.rs:40`.
- Load restores healthcheck-style metadata into daemon image state: `dd-daemon/src/images/transfer/load.rs:18`.
- Save builds the manifest from a subset of image fields and relies on defaults for omitted metadata: `dd-daemon/src/images/transfer/save.rs:38`.

Why this is bad:

An image saved and loaded through dd can lose lifecycle metadata that the manifest type and load path otherwise know how to carry.

Verification:

Create an image with stop signal, volumes, and healthcheck metadata, save/load it, and inspect the restored config.

## `COPY --from=<external-image>` Is Rejected

Priority: P2
Impact: Dockerfile compatibility gap for common multi-image copy pattern
Confidence: High

Evidence:

- Cache descriptor only resolves `--from` through build stage names: `dd-daemon/src/build/steps.rs:31`.
- `copy_step` returns unknown stage for any non-stage `--from`: `dd-daemon/src/build/steps.rs:208`.

Why this is bad:

Docker supports `COPY --from=alpine:latest /bin/busybox /busybox`. dd rejects external image references that are not named stages.

Verification:

Build a Dockerfile using `COPY --from=<existing local image>` and assert it copies from that image rootfs.

## Docker Cp Stat Header Mis-Encodes Special Mode Bits

Priority: P3
Impact: `X-Docker-Container-Path-Stat` can report wrong setuid/setgid/sticky modes
Confidence: Medium

Evidence:

- `go_filemode` keeps raw Unix `0o7777` low bits and only adds directory/symlink type bits: `dd-daemon/src/archive/overlay.rs:113`.

Why this is suspicious:

Docker's stat header uses Go `os.FileMode`, where setuid/setgid/sticky are represented by high mode bits, not raw Unix special bits in the low permission field.

Verification:

Create setuid/setgid/sticky files and compare dd's base64 stat header against Docker's.

## Built Image History Is Synthetic

Priority: P1
Impact: Dockerfile instruction history is lost
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-docker-history-20260710`.

Evidence:

- Image history always returns one `CreatedBy: "dd import"` row: `dd-daemon/src/images/query.rs:38`.
- Build does not persist per-instruction history: `dd-daemon/src/build/handler.rs:458`.

Why this is bad:

`docker history` should expose Dockerfile-created rows for `FROM`, `ENV`, `LABEL`, `CMD`, and other instructions. A synthetic single row hides provenance and makes cache/debug tooling inaccurate.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-docker-history-target CARGO_HOME=/Users/x/.cargo HOME=/Users/x/dd/dd-audit-docker-history-home cargo test -p dd-daemon build_history_preserves_dockerfile_instruction_history -- --nocapture
```

Result: one row only, `CreatedBy="dd import"`.

## Build Cache Seed Ignores Base Image Config

Priority: P1
Impact: downstream cached config steps can replay stale base image metadata
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-docker-history-20260710`.

Evidence:

- Cache seed uses `FROM {base}` plus `rootfs_digest(base_rootfs)`: `dd-daemon/src/build/handler.rs:316`.
- Config-only cache metadata replays `env`, `cmd`, and `labels` from the old hit.

Why this is bad:

Changing a base image's config without changing rootfs content should invalidate downstream cached config. dd can keep stale inherited `Env` such as `FOO=one` after the base changes to `FOO=two`.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-docker-history-target CARGO_HOME=/Users/x/.cargo HOME=/Users/x/dd/dd-audit-docker-history-home cargo test -p dd-daemon build_cache_seed_includes_base_image_env_config -- --nocapture
```

Result: rebuild kept stale `FOO=one`.

## Dockerfile `LABEL` Does Not Merge Base Labels

Priority: P2
Impact: child images lose inherited label metadata
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-docker-history-20260710`.

Evidence:

- `FROM` inherits rootfs, arch, cmd, entrypoint, env, and workdir, but not labels: `dd-daemon/src/build/handler.rs:232`.
- Build clears labels before processing child labels: `dd-daemon/src/build/handler.rs:291`.

Why this is bad:

Docker child images inherit base labels, with child `LABEL` instructions overriding matching keys. dd drops base labels and keeps only child labels.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-docker-history-target CARGO_HOME=/Users/x/.cargo HOME=/Users/x/dd/dd-audit-docker-history-home cargo test -p dd-daemon build_label_inherits_base_image_labels -- --nocapture
```

Result: base label `org.example.base=kept` was absent.

## `.dockerignore` Is Not Applied

Priority: P2
Impact: ignored build-context files can still be copied into images
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-docker-history-20260710`.

Evidence:

- Build context is extracted raw: `dd-daemon/src/build/handler.rs:61`.
- `COPY` resolves directly from the extracted context: `dd-daemon/src/build/steps.rs:207`.

Why this is bad:

Files excluded by `.dockerignore` should be unavailable to `COPY`. dd copies ignored files such as secrets if they are present in the submitted context archive.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-docker-history-target CARGO_HOME=/Users/x/.cargo HOME=/Users/x/dd/dd-audit-docker-history-home cargo test -p dd-daemon build_dockerignore_excludes_context_sources -- --nocapture
```

Result: `secret.txt` listed in `.dockerignore` was copied into `/out/secret.txt`.

## Dockerfile `ENV` Interpolation Ignores Prior ENV

Priority: P1
Impact: image environment values can persist unexpanded
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-dockerfile-cf`.

Evidence:

- Dockerfile parser records instructions without expansion: `dd-images/src/build/dockerfile.rs:10`.
- Build expansion state does not include current stage env for later `ENV`: `dd-daemon/src/build/handler.rs:128`, `dd-daemon/src/build/handler.rs:359`.

Why this is bad:

Docker expands `ENV B=${A}` from the current stage environment. dd persists `B=${A}` after `ENV A=one`, silently changing runtime env.

## Pre-FROM `ARG` Leaks Into Stage Scope

Priority: P1
Impact: Dockerfile variable scoping is too permissive
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-dockerfile-cf`.

Evidence:

- Global build args are retained across parsing: `dd-daemon/src/build/handler.rs:103`.
- Stage expansion uses those args after `FROM`: `dd-daemon/src/build/handler.rs:328`.

Why this is bad:

Pre-`FROM` `ARG` values are available to `FROM` but not later stage instructions unless redeclared. dd lets `COPY file-${VERSION}` after `FROM` use a pre-`FROM` value and succeed.

## Dockerfile `SHELL` Is Ignored

Priority: P1
Impact: shell-form `RUN` and `CMD` use the wrong shell
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-dockerfile-cf`.

Evidence:

- `SHELL` parses as an instruction: `dd-images/src/build/dockerfile.rs:83`.
- Shell-form `CMD` and `RUN` still use `/bin/sh -c`: `dd-daemon/src/build/handler.rs:376`, `dd-daemon/src/build/steps.rs:144`.

Why this is bad:

After `SHELL ["/bin/bash","-c"]`, shell-form commands should use bash. dd still emits `["/bin/sh","-c", ...]`.

## Dockerfile `ONBUILD` Triggers Are Ignored

Priority: P1
Impact: child builds miss base-image trigger instructions
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-dockerfile-cf`.

Evidence:

- Build handler does not execute inherited ONBUILD triggers after `FROM`: `dd-daemon/src/build/handler.rs:383`.

Why this is bad:

Docker executes base-image `ONBUILD` triggers immediately after `FROM`. dd child builds from an image containing `ONBUILD ENV TRIGGERED=yes` had empty env.

## Unknown Build Target Builds Last Stage

Priority: P1
Impact: requested `--target` typos can publish the wrong stage
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-dockerfile-cf`.

Evidence:

- Target handling tracks stage names but does not reject absent targets: `dd-daemon/src/build/handler.rs:303`, `dd-daemon/src/build/handler.rs:420`.

Why this is bad:

Docker errors when the requested target stage does not exist. dd succeeds and tags the final stage, which can publish the wrong image.

## Failed Build Leaves Partial Image Directory

Priority: P2
Impact: failed builds leave stale image output
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-dockerfile-cf`.

Evidence:

- Build writes into the image output before all steps have succeeded: `dd-daemon/src/build/handler.rs:352`.

Why this is bad:

Failed builds should clean partial output. dd left `images/failcleanup_latest` after `COPY missing.txt` failed.

## Exec-Form JSON Drops Non-String Elements

Priority: P2
Impact: invalid Dockerfile JSON arrays are silently truncated
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-dockerfile-cf`.

Evidence:

- Exec-form parser filters JSON array elements to strings: `dd-images/src/build/dockerfile.rs:86`.

Why this is bad:

`["echo", 123]` is invalid exec-form JSON for Dockerfile commands. dd parses it as `["echo"]` instead of rejecting the Dockerfile.

## Fixed Per-Process Temp Dirs Race Concurrent Operations

Priority: P1
Impact: concurrent build/push/pull operations can trample shared temp paths
Confidence: Medium-high

Evidence:

- Build context uses `.build-ctx-<pid>`: `dd-daemon/src/build/handler.rs:52`.
- Push uses `.push-<pid>`: `dd-daemon/src/images/transfer/push.rs:45`.
- Pull layer temp files use `dd-layer-<pid>-<layerid>`: `dd-images/src/registry/client/pull.rs:99`.

Why this is bad:

The daemon can serve concurrent requests within one process. PID-based names are not unique across concurrent requests, so two operations can remove, overwrite, or read each other's staging directories.

Verification:

Run two builds/pushes/pulls concurrently against the same daemon process with different inputs and assert staging paths are unique and results are isolated.

## Dockerfile USER Is Ignored In Build Output

Priority: P2
Impact: image runtime identity metadata disappears
Confidence: High

Evidence:

- The Dockerfile handler accepts `USER` but falls through to ignored metadata handling: `dd-daemon/src/build/handler.rs:383`.
- Final metadata write omits the user field: `dd-daemon/src/build/handler.rs:458`.

Why this is bad:

`USER` should affect default runtime identity and image inspection. Builds can succeed while producing images that later run as the wrong user.

## FROM Local Lookup Ignores Tag

Priority: P2
Impact: builds can use the wrong local base image
Confidence: Medium-high

Evidence:

- Local base lookup matches only `ref_name`: `dd-daemon/src/build/handler.rs:246`.

Why this is bad:

Two local tags that share a repository name can point at different images. Ignoring the tag can make a build start from the wrong rootfs while the Dockerfile names a specific tag.

## Tag/Digest Reporting Is Synthetic And Inconsistent

Priority: P2
Impact: image clients see unstable IDs and unusable repo digests
Confidence: Medium-high

Evidence:

- Image listing uses a synthetic id: `dd-daemon/src/images/query.rs:22`.
- Repo digests are empty in list output: `dd-daemon/src/images/query.rs:28`.
- Inspect also reports a fake id: `dd-daemon/src/images/query.rs:115`.
- Distribution and pull paths synthesize digests separately: `dd-daemon/src/images/query.rs:76`, `dd-daemon/src/images/pull/stream.rs:79`.

Why this is bad:

Clients use image IDs and repo digests for cache identity, deployment pinning, and prune decisions. Synthetic or inconsistent values can make automation treat the same image as different or miss digest-pinned behavior.

## `--platform` Ignores OS Prefix

Priority: P1
Impact: non-Linux platform requests silently map to Linux pulls
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-X-archive-followup-20260710`.

Evidence:

- Platform parsing scans slash-separated segments for `amd64` or `arm64`: `dd-daemon/src/images/pull/arch.rs:29`.
- The supported architecture list is built from that match without validating OS: `dd-daemon/src/images/pull/arch.rs:40`.

Why this is bad:

Requests such as `windows/amd64` or `nonsense/amd64` should be rejected for a Linux daemon or treated as unsupported. dd silently turns them into Linux amd64 pulls.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-X-archive-followup-20260710-target cargo test -p dd-daemon platform_arch_rejects_non_linux_os_even_with_supported_arch -- --nocapture
```

Result: failed; `windows/amd64` mapped to `Some("amd64")`.

## Docker Push Drops Runtime Metadata From OCI Config

Priority: P1
Impact: pushed images lose entrypoint, env, user, workdir, ports, labels, volumes, stop signal, and healthcheck metadata
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-BI2-copy`.

Evidence:

- Daemon image state carries runtime metadata in `dd-daemon/src/model/wire/image.rs:8`.
- Push passes only `img.cmd`, architecture, and OS to the registry path: `dd-daemon/src/images/transfer/push.rs:47`.
- The registry client serializes only `config.Cmd`: `dd-images/src/registry/client/push.rs:26`.

Why this is bad:

Registry push should preserve the OCI image config. Dropping runtime metadata silently changes how pulled images start and how tools inspect them.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-BI2-target cargo test -p dd-daemon poc_push_preserves_image_runtime_metadata_in_config_blob -- --ignored --nocapture
```

Result: failed at the captured config assertion; `Entrypoint` was `Null`, expected `["/entry"]`.

## Source-Inferred: Darwin Jail Symlink Semantics Can Produce Wrong Contents

Priority: P3
Impact: possible wrong-content behavior for macOS-container paths
Confidence: Medium

Evidence:

- Darwin jail maps guest absolute paths to host paths by string after canonicalizing `.` and `..`: `dd-jit-darwin/src/runtime/os/darwin/jail/jail.c:340`.
- Host `open`/`stat` then follows symlinks unless a specific call uses nofollow behavior.

Why this is suspicious:

Linux VFS resolution has a component-walk resolver that clamps symlinks in the guest namespace. The Darwin jail path is simpler and may silently become host symlink semantics for symlinked rootfs paths.

Verification needed:

Create a macOS-container rootfs with symlinks that point outside the root and compare `open`, `stat`, and write behavior against expected container path semantics.

## Image VOLUME Copy-Up Can Escape The Image Rootfs

Priority: P1
Impact: anonymous volume seeding can copy files from outside the image rootfs
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-build-output-20260710`.

Evidence:

- Anonymous volume copy-up joins the image rootfs with the image `VOLUME` target after only trimming a leading slash: `dd-daemon/src/containers/lifecycle/create/volumes.rs:49`.
- Container create feeds image volume targets into that path: `dd-daemon/src/containers/lifecycle/create/mod.rs:214`.

Why this is bad:

Image volume targets should be confined to the image rootfs or rejected when they contain invalid path traversal. dd can seed an anonymous volume from `rootfs/../outside`, silently copying contents that were not part of the image filesystem.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-build-output-20260710-target cargo test -p dd-daemon audit_image_volume_copyup_cannot_escape_image_rootfs -- --nocapture
```

Result: the anonymous volume was seeded with `sentinel.txt` from `rootfs/../outside`.

## Relative WORKDIR Dotdot Persists A Different Config Path

Priority: P2
Impact: build creates one directory but stores another runtime working directory
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-build-output-20260710`.

Evidence:

- Dockerfile `WORKDIR` handling stores the joined logical path before normalizing `..`: `dd-daemon/src/build/handler.rs:367`.

Why this is bad:

`WORKDIR ../c` from `/a/b` creates `/a/c` through host path normalization, but dd persists `Config.WorkingDir` as `/a/b/../c`. Later container start can try to chdir through a path whose intermediate directory does not exist, even though the build created the normalized target.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-build-output-20260710-target cargo test -p dd-daemon audit_build_workdir_dotdot_config_matches_created_rootfs_dir -- --nocapture
```

Result: `Config.WorkingDir` was `/a/b/../c`; expected `/a/c`.

## ENV Override Moves Inherited Keys To The End

Priority: P2
Impact: inherited image config order changes when one key is overridden
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-build-output-20260710`.

Evidence:

- Dockerfile `ENV` override removes an existing key with `retain` and appends the replacement: `dd-daemon/src/build/handler.rs:359`.

Why this is bad:

Image config environment order is observable and Docker replaces overridden keys in place while appending only new keys. dd moves overridden inherited keys to the end, producing a different final config and breaking clients/tests that compare image config deterministically.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-build-output-20260710-target cargo test -p dd-daemon audit_build_env_override_preserves_original_key_position -- --nocapture
```

Result: observed `["B=base", "Z=last", "A=new", "C=child"]`; expected `["A=new", "B=base", "Z=last", "C=child"]`.

## UID/GID Metadata Is Lost On Load/Import/Cp PUT

Priority: P1
Impact: extracted image/container files silently change ownership
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-archive-meta-20260710`.

Evidence:

- Image load shells out to plain `tar xf`: `dd-images/src/image/archive/load.rs:21`.
- Image import uses the same unprivileged extraction pattern: `dd-images/src/image/archive/import.rs:19`.
- Archive PUT extraction uses daemon-side tar extraction: `dd-daemon/src/archive/handlers.rs:172`.

Why this is bad:

Docker archives preserve numeric owner metadata in image and container filesystems. dd extracts as the host daemon user, so files owned by `1234:2345` become `501:501` on this host, silently changing runtime permissions and ownership-sensitive behavior.

Isolated proof:

```sh
cd /Users/x/dd/dd-audit-archive-meta-20260710
audit/archive_metadata_probes.sh
```

Observed:

```text
archive metadata: -rw-r----- 1234/2345 ... rootfs/payload
load observed uid:gid=501:501
import observed uid:gid=501:501
cp observed uid:gid=501:501
```

## Save/Cp GET Truncate Nanosecond Mtimes

Priority: P2
Impact: tar round-trips lose subsecond timestamp metadata
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-archive-meta-20260710`.

Evidence:

- Image save shells out to `tar cf`: `dd-images/src/image/archive/save.rs:22`.
- Archive GET uses tar output without pax/nanosecond preservation: `dd-daemon/src/archive/handlers.rs:80`.

Why this is bad:

Subsecond mtimes are significant for build caches, incremental sync, and reproducibility tooling. dd emits tar streams that round `...05.987654321` down to `...05.000000000`.

Isolated proof:

```sh
cd /Users/x/dd/dd-audit-archive-meta-20260710
audit/archive_timestamp_probe.sh
```

Result: `FAIL nanosecond mtime lost; expected fractional ns 987654321, observed 000000000`.

