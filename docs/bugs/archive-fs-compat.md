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

## Sparse Files Expand Through Save/Push Tar Paths

Priority: P2
Impact: archive bloat and slow image transfer for sparse data
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-L-archive-storage-20260710`.

Evidence:

- Image save uses plain `tar cf`: `dd-images/src/image/archive/save.rs:22`.
- Push uses `tar cf - | gzip -n`: `dd-images/src/registry/layer.rs:132`, called from `dd-images/src/registry/client/push.rs:23`.

Why this is bad:

Sparse files should remain sparse or at least avoid expanding holes into full zero runs during archive creation. Current save of a 64 MiB sparse file produced a 67,112,960 byte archive.

Isolated proof:

```sh
cargo test -p dd-images save_archive_preserves_sparse_file_without_expanding_holes -- --nocapture
```

Result: failed; archive length was `67112960`.

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

## Build Cache Digests Break On Apostrophes In Paths

Priority: P2
Impact: cache keys can become empty or wrong for valid host paths
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/worker-Q/dd-Q3`.

Evidence:

- `rootfs_digest` interpolates paths into a shell script with single quotes: `dd-images/src/build/cache.rs:48`.
- `path_digest` uses the same shell-quoted shape: `dd-images/src/build/cache.rs:72`.

Why this is bad:

Host directories and build contexts can legally contain apostrophes. A cache digest helper that breaks shell quoting can produce an empty digest or wrong cache identity, causing missed cache invalidation or broken builds.

Isolated proof:

```sh
cargo test -p dd-images path_digest_handles_single_quote_in_path --target-dir /Users/x/dd/worker-Q/target-Q
```

Result: failed; digest length was `0` instead of `64`.

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

## Dockerfile `# escape=` Directive Is Ignored

Priority: P2
Impact: valid Dockerfiles with non-default continuation syntax parse incorrectly
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-dockerfile-cf`.

Evidence:

- Parser uses fixed backslash continuation behavior: `dd-images/src/build/dockerfile.rs:60`.

Why this is bad:

`# escape=\`` should make backtick the continuation character. dd split a continued command into separate `RUN` and `&&` instructions.

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

## Platform Selection Discards OCI Variant

Priority: P2
Impact: registry pulls can select the wrong architecture variant
Confidence: Medium

Evidence:

- Registry pull platform selection checks architecture and OS but discards OCI `variant`: `dd-images/src/registry/client/pull.rs:130`.

Why this is bad:

Architectures such as arm can require variant-specific manifests. Ignoring the field can pull a compatible-looking but wrong binary set.

## Digest-Pinned References Are Parsed As Tags

Priority: P1
Impact: digest-pinned pulls resolve the wrong repository and manifest path
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-X-archive-followup-20260710`.

Evidence:

- Registry reference parsing calls `split_tag`: `dd-images/src/registry/reference.rs:19`.
- `alpine@sha256:<hex>` is split at the digest colon: `dd-images/src/registry/reference.rs:90`.
- Pull resolves manifests through `/manifests/{self.image.tag}`: `dd-images/src/registry/client/pull.rs:59`.

Why this is bad:

Digest references are the normal way to pin exact image content. Misparsing the digest as repository/tag rewrites `alpine@sha256:<hex>` into repository `library/alpine@sha256` and tag `<hex>`, so the pull does not request the pinned digest.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-X-archive-followup-20260710-target cargo test -p dd-images digest_reference_does_not_rewrite_digest_into_repo_and_tag -- --nocapture
```

Result: failed; repository parsed as `library/alpine@sha256`.

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

## Pull Does Not Verify Downloaded Blob Digests

Priority: P1
Impact: registry content mismatch can be accepted as valid image data
Confidence: Medium-high

Evidence:

- Config blob path reads by digest and parses JSON without rehashing bytes: `dd-images/src/registry/client/pull.rs:78`.
- Layer path downloads and extracts blobs without comparing the downloaded bytes to the advertised digest: `dd-images/src/registry/client/pull.rs:97`.
- A file hashing helper already exists: `dd-images/src/image/digest.rs:20`.

Why this is bad:

Content-addressed pulls rely on verifying that bytes match the digest used to address them. Accepting a valid but wrong gzip or config body can silently produce the wrong image contents.

Verification:

Serve a manifest whose config/layer digest points to one hash while the blob endpoint returns different valid bytes, then assert pull rejects before extraction or metadata persistence.

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AB-archive-registry-build-20260710`.

Isolated proof:

```sh
cargo test -p dd-images poc_pull_rejects_layer_blob_digest_mismatch_before_extracting -- --ignored --nocapture
```

Result: failed; pull returned `Ok(LocalImage ...)`, emitted `PullComplete`, and extracted `from-bad-blob.txt` from mismatched blob bytes.

## Pull Ignores Config `rootfs.diff_ids`

Priority: P1
Impact: registry pull can install layer content that does not match the image config
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-BP2-registry-metadata-20260710`.

Evidence:

- Pull reads the OCI config: `dd-images/src/registry/client/pull.rs:21`.
- It unpacks manifest layers by compressed digest only: `dd-images/src/registry/client/pull.rs:48`.
- No check ties uncompressed layer content to `config.rootfs.diff_ids`.

Why this is bad:

OCI image identity includes uncompressed layer digests in config `rootfs.diff_ids`. Pulling by compressed descriptor digest alone can install content that does not match the config, corrupting image identity and cache/provenance assumptions.

Isolated proof:

```sh
cd /Users/x/dd/dd-audit-BP2-registry-metadata-20260710
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-BP2-registry-metadata-20260710-target cargo test -p dd-images poc_pull_must_validate_config_diff_ids_against_layers -- --ignored --nocapture
```

Result: pull returned success and installed the file even though `config.rootfs.diff_ids[0]` did not match the uncompressed layer digest.

## Opaque Whiteout Pre-Pass Can Remove Paths Outside Rootfs

Priority: P1
Impact: malformed layer can cause host-side data loss during pull
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AB-archive-registry-build-20260710`.

Evidence:

- Opaque marker discovery accepts parent paths such as `../outside`: `dd-images/src/registry/layer.rs:59`.
- The pre-pass joins discovered dirs to `rootfs` and removes them before extraction: `dd-images/src/registry/layer.rs:86`.

Why this is bad:

Layer whiteout handling should be contained to the image rootfs. Accepting `..` in an opaque marker path lets the cleanup pre-pass delete files outside the rootfs before tar extraction even begins.

Isolated proof:

```sh
cargo test -p dd-images poc_opaque_marker_paths_must_not_clear_outside_rootfs -- --ignored --nocapture
```

Result: failed; `outside/keep.txt` was deleted.

## Registry Layer Extraction Follows Existing Rootfs Symlinks

Priority: P1
Impact: pulled layers can write outside the image rootfs
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AJ-archive-registry-20260710`.

Evidence:

- Pull applies layers by calling `extract_targz` directly after the opaque pre-pass: `dd-images/src/registry/client/pull.rs:120`.
- `extract_targz` runs host `tar -xzf ... -C rootfs`: `dd-images/src/registry/http/archive.rs:30`.

Why this is bad:

If an earlier layer or base rootfs contains `linkout -> ../outside`, a later layer entry `linkout/file.txt` can be written through that symlink by host tar. Layer extraction must be contained to the image rootfs or reject the archive.

Isolated proof:

```sh
TMPDIR=/Users/x/dd/dd-worker-AJ-archive-registry-20260710-target/tmp CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AJ-archive-registry-20260710-target cargo test -p dd-images poc_layer_extract_must_not_write_through_existing_rootfs_symlink -- --ignored --nocapture
```

Result: failed; `linkout/file.txt` was written to `outside/file.txt` through `rootfs/linkout -> ../outside`.

## Pull Accepts Invalid Config Blobs As Empty Config

Priority: P1
Impact: image runtime metadata can be silently lost
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AJ-archive-registry-20260710`.

Evidence:

- Pull resolves the manifest and config descriptor: `dd-images/src/registry/client/pull.rs:21`.
- It swallows `config_blob` errors with `unwrap_or_else(|_| json!({}))`: `dd-images/src/registry/client/pull.rs:22`.
- The config reader would otherwise report missing or invalid config: `dd-images/src/registry/client/pull.rs:78`.

Why this is bad:

Invalid or missing config should reject the image. Treating it as `{}` silently drops `Cmd`, `Entrypoint`, `Env`, `WorkingDir`, `User`, and other config-derived identity.

Isolated proof:

```sh
TMPDIR=/Users/x/dd/dd-worker-AJ-archive-registry-20260710-target/tmp CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AJ-archive-registry-20260710-target cargo test -p dd-images poc_pull_must_reject_invalid_config_blob -- --ignored --nocapture
```

Result: failed; `Client::pull` returned `Ok(Pulled { config: {} })` after the fake registry served `not json`.

## Registry Push Layer Packaging Breaks On Apostrophes In Paths

Priority: P2
Impact: valid rootfs paths can make push packaging fail
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AP-archive-registry-layer-20260710`.

Evidence:

- `tar_gzip` builds a shell string with single-quoted paths: `dd-images/src/registry/layer.rs:132`, `dd-images/src/registry/layer.rs:133`.
- Registry push calls that layer packaging helper: `dd-images/src/registry/client/push.rs:23`.

Why this is bad:

Filesystem paths can contain apostrophes. Push should pass paths as process arguments or quote them safely; otherwise an ordinary store path can break layer packaging.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AP-archive-registry-layer-20260710-target cargo test -p dd-images poc_tar_gzip_handles_single_quote_in_rootfs_path -- --ignored --nocapture
```

Result: failed with `sh: 1: Syntax error: Unterminated quoted string`.

## Concurrent Registry Manifest PUTs Share One Temp Body File

Priority: P1
Impact: concurrent pushes can upload the wrong or empty manifest
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AV-archive-load-save-push-registry-20260710`.

Evidence:

- `put_bytes` writes every request body to `/tmp/dd-reg-body-<pid>.bin`: `dd-images/src/registry/http/verbs.rs:60`.
- Registry push uses that helper for manifest upload: `dd-images/src/registry/client/push.rs:83`.

Why this is bad:

Two concurrent pushes in one daemon process can overwrite or remove each other's manifest body after one request has spawned curl but before curl reads the file. This can publish the wrong manifest or an empty body.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AV-archive-load-save-push-registry-20260710-target cargo test -p dd-images poc_put_bytes_uses_unique_temp_body_per_upload -- --ignored --nocapture
```

Result: failed; the first upload captured an empty body instead of `first manifest body`.

## Pull Accepts Invalid Manifest Schema Version

Priority: P1
Impact: invalid registry manifests can be treated as valid images
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-oci-audit`.

Evidence:

- Manifest resolution returns any non-index JSON as a single manifest: `dd-images/src/registry/client/pull.rs:21`.
- Pull never validates `schemaVersion` or manifest media type before extraction: `dd-images/src/registry/client/pull.rs:59`.

Why this is bad:

OCI/Docker image manifests should use schema version 2 and a supported media type. Accepting schema version 1 can pull malformed or incompatible metadata as if it were valid.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-oci-audit-target cargo test -p dd-images audit_pull_rejects_manifest_schema_version_other_than_two -- --ignored --nocapture
```

Result: pull returned `Ok`; expected rejection before layer extraction.

## Unsupported Layer Media Type Is Unpacked As Gzip

Priority: P1
Impact: pull ignores declared compression/format
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-oci-audit`.

Evidence:

- Layer descriptors are read from the manifest: `dd-images/src/registry/client/pull.rs:31`.
- Pull passes every layer to gzip tar extraction: `dd-images/src/registry/client/pull.rs:120`.
- Registry media types include unsupported compression such as zstd: `dd-images/src/registry/mod.rs:42`.

Why this is bad:

A layer declaring `application/vnd.oci.image.layer.v1.tar+zstd` should be rejected or decompressed as zstd. dd ignores the media type and unpacks the bytes as gzip if they happen to be gzip.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-oci-audit-target cargo test -p dd-images audit_pull_rejects_unsupported_layer_media_type -- --ignored --nocapture
```

Result: pull returned `Ok` for a zstd media type whose bytes were gzip.

## Config And Layer Descriptor Sizes Are Not Enforced

Priority: P1
Impact: registry pull accepts truncated or extra blob bytes
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-oci-audit`.

Evidence:

- Manifest config and layer descriptors contain sizes: `dd-images/src/registry/client/pull.rs:31`.
- Config size is ignored while reading JSON: `dd-images/src/registry/client/pull.rs:78`.
- Layer size is used for progress totals, not checked against downloaded blob length: `dd-images/src/registry/client/pull.rs:101`.

Why this is bad:

Descriptor sizes are part of registry integrity checks. Ignoring them can accept truncated, padded, or otherwise mismatched blobs.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-oci-audit-target cargo test -p dd-images audit_pull_rejects_descriptor_size_mismatch -- --ignored --nocapture
```

Result: pull returned `Ok`; expected rejection when config/layer descriptor sizes did not match actual bytes.

## Valid Zero-Layer Manifests Are Rejected

Priority: P2
Impact: scratch-style empty images cannot be pulled
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-oci-audit`.

Evidence:

- Pull rejects `layers: []` as `manifest has no layers`: `dd-images/src/registry/client/pull.rs:23`.

Why this is bad:

OCI images can have zero filesystem layers when the config rootfs has no diff IDs. Rejecting them breaks empty/scratch-style images.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-oci-audit-target cargo test -p dd-images audit_pull_accepts_valid_zero_layer_manifest -- --ignored --nocapture
```

Result: returned `Manifest("manifest has no layers")`; expected an empty rootfs.

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

## Failed Registry Pull Leaves Partial Final Rootfs

Priority: P1
Impact: failed pulls can leave an image path populated with only earlier layers
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-registry-compression-20260710`.

Evidence:

- Pull resets the final image rootfs before pulling layers: `dd-images/src/registry/client/pull.rs:43`.
- Later layer failure returns immediately without cleaning already-extracted final rootfs contents: `dd-images/src/registry/client/pull.rs:48`.

Why this is bad:

A failed pull should leave no usable final image rootfs or should roll back to the previous image. dd extracts earlier layers into the final target, then returns an error on a later layer, leaving partial image contents behind.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-registry-compression-20260710-target cargo test -p dd-images poc_failed_later_layer_pull_must_not_leave_partial_final_rootfs -- --ignored --nocapture
```

Observed:

```text
partial_file=true rootfs_exists=true
err=tar extract failed: gzip: stdin: not in gzip format
```

## Layer Downloads Treat HTTP Error Bodies As Blobs

Priority: P2
Impact: registry HTTP failures are reported as downloaded layers and then fail later as tar data
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-registry-compression-20260710`.

Evidence:

- Layer download uses `curl -sSL -o` without `-f` or explicit status capture: `dd-images/src/registry/http/verbs.rs:82`.

Why this is bad:

HTTP 404/500 layer responses should fail as registry/blob errors before download completion and extraction. dd can save the error body as a layer, emit `DownloadComplete`, and then fail during gzip/tar extraction with a misleading partial state.

Isolated proof:

```text
events=[..., PullComplete { id: "111111111111" }, ..., DownloadComplete { id: "222222222222" }, Extracting ...]
```
