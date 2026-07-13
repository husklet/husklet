# Storage and serialization compatibility audit — wave Z (2026-07)

Documentation-only audit of workspace configuration, daemon state/wire models, image sidecars/archive
manifests, tag aliases, and build/runtime caches. No repository-local `AGENTS.md` or
`.dev/AGENTS.local.md` exists in this worktree.

## Conclusion: no persisted field is an immediate safe cut

Every apparent duplicate in the durable models serves a distinct execution, wire, migration, or recovery
path. Removing fields for file-size aesthetics would risk restart correctness and provides no meaningful
speed gain. The safe work is consolidating representations and making migrations explicit, then retiring
old readers only after a measured support window.

## Workspace configuration

`WorkspaceStore` writes one canonical block format but reads two representations: `[workspace]` key/value
blocks and legacy `name<TAB>arch<TAB>image` rows. Git history places the legacy reader/new block migration
in `5cc10571` (2026-07-06); no current writer emits tab rows. It is therefore a migration reader, not a
duplicate live format.

Safe retirement boundary:

1. on successful load of any legacy row, rewrite the whole store through the canonical block writer;
2. record/test that aliases for architecture still canonicalize through `Arch::as_str`;
3. retain the reader for at least the declared compatibility window/release;
4. remove only the `cur.is_none() && line.contains('\t')` branch and its `splitn(3, '\t')` parsing after
   telemetry/user policy says pre-block files are no longer supported.

Do not remove `Arch` aliases, VPN/CUDA parse aliases, default-on `docker_sock`, or omitted false/None fields:
they are CLI/config compatibility. `VpnConfig` and `CudaDevice` are compact string encodings nested inside
the line format; replacing them with JSON/TOML adds dependencies and parsing work without eliminating a
second persisted file.

`Mount` is represented structurally in memory and as `host:container:mode` on disk. That conversion is
necessary, but the colon-split format is ambiguous for colon-containing host paths. A future escaped/versioned
format requires dual-read migration; it is not a reason to delete fields.

## Daemon state versus Docker wire models

The daemon deliberately uses one persisted `Container` model plus separate API response structs. This looks
duplicated, but API structs enforce Docker key spelling/shape while persisted state carries restart-only
data. Merging them would couple disk migrations to Docker API evolution and increase serialization/copy
risk.

Specific duplicate-looking fields that must remain:

- `cmd` is the resolved launch argv; `entrypoint` + `cmd_config` preserve Docker inspect/commit separation.
- `started_at`/`finished_at` seconds drive sorting/log time filters and stable integer state;
  `started_at_ns`/`finished_at_ns` preserve nanosecond inspect/restart ordering.
- `image`, `rootfs`, and persisted `arch` jointly recover after tag changes or image deletion. `load_state`
  first matches rootfs, then image ref, and uses persisted arch when neither resolves; tests lock all cases.
- `stdout`/`stderr` and `netns_key` are correctly `#[serde(skip)]`; logs live in separate files to avoid
  rewriting/bloating state, while netns is runtime-only.
- metadata-only Docker fields (log config, capabilities, devices, security options) round-trip through
  inspect even when the JIT does not enforce them.

`save_state_checked` clones containers/volumes/networks into `Persisted` before pretty JSON serialization.
That is one request-time copy, but it releases no compatibility benefit to eliminate it by serializing
`Inner`: `Inner` also owns live Tokio channels, execs and mutex-backed state that must never persist. If
write cost becomes measurable, snapshot only borrowed serializable slices/maps or move persistence off the
hot lock; do not derive serialization on `Inner`.

The state file is atomically replaced. Keep the temp-write/rename and checked-vs-best-effort writer split:
durability-sensitive handlers use errors for rollback while routine updates remain available despite disk
issues. They are distinct semantics, not redundant helpers.

## Image representations

Images have four legitimate representations:

1. OCI registry config/manifest during pull;
2. store-local `dd-image.json` optimized for startup discovery;
3. archive `dd-manifest.json` for save/load portability;
4. daemon runtime `Image`, converted from runtime-agnostic `DiscoveredImage`.

The two sidecars overlap fields but have different lifecycle/ownership. Store metadata may be enriched in
place (environment recovery); archive metadata must be a stable portable contract. The daemon conversion
keeps `dd-images` independent of daemon health/runtime types. Collapsing all four would violate crate
boundaries or require reparsing OCI data at every daemon startup, regressing speed.

There is nevertheless avoidable schema duplication: discovery manually indexes a `serde_json::Value`,
archive uses typed `Manifest`, and multiple writers construct `dd-image.json`. Introduce a typed, versioned
`StoreMetadata` in `dd-images`, with defaults matching current `Value` behavior, and route pull/build/import/
load/env-enrichment writers through it. Preserve unknown keys when enriching old sidecars so forward data
is not discarded. This reduces conversions without removing disk fields.

Legacy/fallback readers are evidence-backed:

- missing store sidecar falls back to directory name + binary sniffing;
- old/pre-seeded sidecars missing env recover OCI config once and persist it for faster later starts;
- archive load supports rootfs-only dd archives and standard Docker-save archives;
- missing OS defaults to Linux, while explicit unsupported OS is rejected;
- missing arch probes the rootfs then falls back to arm64 for unprobeable historical data.

Keep these until inputs are explicitly unsupported. The one-time env enrichment write is a performance
optimization, not stale mutation.

## Tag aliases

`dd-aliases/*.json` is separate from image sidecars because multiple tags may share one rootfs and bundled/
read-only image directories cannot be rewritten. It was introduced in `251e9eb9` (2026-07-10) to persist
previously in-memory-only tags. It is current, not a stale migration format. Merging alias names into a
single sidecar would lose one-to-many identity or require mutating immutable images.

Add schema/version validation and remove alias files when tags are deleted, but keep the representation.
Alias discovery's clone of the base image is a cheap in-memory metadata copy and avoids duplicating rootfs
data.

## Caches and manifests

Build-cache `meta.json` stores cumulative configuration beside optional rootfs snapshots so a hit avoids
re-executing steps. JIT pcache, image-size memoization, and image/build caches have distinct keys and
invalidations; none is a duplicate cache.

Keep full rootfs snapshots for filesystem-mutating build steps: replacing them with reconstruction would
slow cache hits. Non-filesystem steps already avoid snapshots. The image-size process cache is keyed by
rootfs and assumes immutable images; invalidate on image mutation/removal rather than deleting the cache.

Archive `Manifest` defaults are compatibility-critical. Its test correctly proves `{}` is invalid because
`name` is required; the comment now states that only non-identity fields default. Add oldest-version fixture
tests before changing any `skip_serializing_if` behavior. Omitted optional fields reduce archive size while
defaults preserve old loads.

## Exact safe text/helper cleanup

- Remove historical comments claiming an older representation after typed migration is complete, but keep
  why rootfs/image/arch matching order matters.
- Centralize store-sidecar field names/conversions in typed metadata; delete duplicate JSON indexing/writer
  helpers only after byte-equivalence tests.
- Add explicit format-version constants and fixtures for workspace blocks, state JSON, store sidecars,
  archive manifests, aliases, and build-cache metadata. Generate schema inventories from those fixtures.

No current persisted field, migration reader, cache, or manifest is authorized for immediate deletion.
