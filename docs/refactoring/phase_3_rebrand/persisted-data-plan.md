# Husklet persisted data and external compatibility plan

Branding stored on disk cannot be treated like source identifiers. This plan enumerates each durable or
externally resolved surface and the safe choices. Final policy must be selected in `decisions.md` before R5.

## State roots and configuration

| Surface | Current location/name | Husklet target | Required behavior |
|---|---|---|---|
| main state root | `~/.dd` | `~/.husklet` | new default; detect old root and give explicit migration/purge guidance |
| daemon socket | `run/docker.sock` under root | same basename under new root | Docker compatibility; no connection to old daemon by accident |
| daemon state | `state.json` | same basename | schema fields remain unless independently versioned; copy/migrate atomically |
| images/volumes | root subdirectories | same semantic subdirs | preserve ownership/permissions/hardlinks and image aliases |
| workspaces | `workspaces.conf` and workspace directories | under new root | legacy tab reader policy remains separate from root migration |
| terminal config/defaults | `term.conf`, `term-defaults.conf` | under new root | preserve contents; update generated comments/help paths |
| GUI data/NVML/drop-ins | `gui`, `nvml`, `bin` subtrees | under new root | rebuild generated shims when paths/artifact names change |
| pcache/buildcache | `pcache`, `buildcache` | under new root | invalidate by version/config identity; do not blindly copy stale compiled pointers |
| container fs generation/state | container subtrees | under new root | preserve restart ordering and atomic state writes |
| logs | `~/Library/Logs/dd` | `~/Library/Logs/husklet` | updater/snapshot/uninstall agree; old logs may remain read-only |

Fresh cutover does not mean silent data loss. On startup:

1. if only `~/.husklet` exists, use it;
2. if neither exists, initialize Husklet;
3. if only `~/.dd` exists, stop with one actionable migration/fresh-start command unless an explicit
   migration mode was selected;
4. if both exist, never merge automatically—require the operator to select one.

## Image and archive metadata

`dd-image.json` is store-local discovery metadata; `dd-manifest.json` is a portable archive contract;
`dd-aliases` represents one-to-many tags. Their names overlap the brand but their compatibility domains
differ.

Safe options:

- **format-compatible rename:** reader accepts old and new filenames, writer emits only a versioned new
  name, conflicting dual files are an error, and enrichment preserves unknown fields;
- **format-version cutover:** introduce a new explicit format/version and conversion command;
- **retain technical filename:** keep old filename as a compatibility identifier and list it in residue.

Never simply rename the filename in writers: existing stores/archives would become invisible. Docker-save
archives and rootfs-only historical archives remain covered by their current fallback behavior.

## Xattrs and filesystem metadata

Engine namespacing uses brand-bearing xattrs (including `user.dd.*`/`user.ddx.*` families); test fixtures
also use arbitrary names such as `user.ddtest`. Classify exact engine-written keys separately from opaque
fixture keys.

For engine keys, choose dual-read/new-write with conflict detection or an explicit filesystem migration.
Copy-up, archive save/load, overlay and cross-container isolation tests must prove uid/gid and arbitrary
user xattrs survive. Do not rename arbitrary guest-created xattrs: they are guest data, not product state.

## JIT pcache and checkpoint identity

Package/symbol/env changes can affect engine path, configuration hash, embedded pointers and serialized
metadata even if instruction translation is unchanged. R5 must bump/invalidate caches whenever identity or
layout changes. A cache miss and rebuild is safe; loading an old incompatible cache is not.

Checkpoint directories may contain executable names, config/wire versions and trigger/pid paths. Treat old
checkpoints as incompatible unless a fixture proves restore across the exact rename. Failure must be typed
and early, not a later crash or timeout.

## macOS external state

- `/Applications/dd.app` and `/Applications/husklet.app` must not both own the same launchd job/context.
- boot out `com.dd.daemon` before installing `com.husklet.daemon`; verify no old process/socket remains.
- create the Husklet Docker context only after its socket is live; remove/retain the old context according
  to uninstall policy.
- updater release names, mounted DMG volume, bundle ID and destination path change together.
- Keychain/notary secret *variable names* can change, but Apple team/profile values are external data.

## Remote/external identifiers

The macOS image reference cannot be cosmetically renamed. Publish and verify a Husklet reference/digest
first, then change defaults. GitHub release asset names and updater URL parsing change in one release wave.
Third-party container image names, Docker Hub namespaces and standard runtime strings remain external unless
the project owns and publishes replacements.

## Migration test matrix

| Starting state | Expected result |
|---|---|
| no old/new data | clean Husklet initialization |
| old root only | explicit stop/guidance or tested migration—never silent empty state |
| new root only | normal launch/restart |
| both roots | explicit conflict error |
| old image sidecar/archive | load/migrate/reject exactly per selected policy |
| unknown future metadata keys | preserved through enrichment/migration |
| old xattrs | read/migrate/reject per key policy; guest arbitrary xattrs untouched |
| old pcache/checkpoint | safe invalidation or typed incompatibility |
| old app/launchd/context installed | controlled uninstall/cutover with no duplicate daemon |

Test filesystem metadata, content, service ownership and subsequent restart—not merely whether a path now
exists.
