# Overlay migration evidence

## Inventory and integrity

This folder contributes one logical case and two guest-ISA rows. The source is
byte-identical to `../engine/tests/compat/overlay/coherence.c` (SHA-256
`77faa9c4d02dbf51af7269a0f35746c3d2a018b53ec44b6eb8a0db4e86d23733`).
The expected stdout is the exact 21-byte string `overlay coherence ok\n`, as
emitted by the retained probe. Both rows use the retained integration build
flags `-static -O2 -std=c11` and expect exit status zero.

The retained inventories disagree only on historical harness scope:
`guest_inventory.tsv` names the AArch64 typed launch, while
`legacy_rust_inventory.tsv` records both AArch64 and x86-64 overlay correctness
lanes. The self-contained YAML therefore enumerates both ISAs and does not hide
the x86-64 obligation.

## Parallel native oracle

Both guest binaries were cross-compiled in parallel and executed concurrently
with `qemu-aarch64` and `qemu-x86_64`. Each QEMU process ran inside a disposable
`bwrap` user/mount namespace whose writable `/etc/hostname` began with `caca`;
this avoids modifying the development environment and supplies the root
identity required by the `fchown` assertion.

The build commands were:

```text
aarch64-linux-gnu-gcc -static -O2 -std=c11 -o target/testing/runtime/overlay/aarch64/coherence tests/runtime/overlay/coherence.c
x86_64-linux-gnu-gcc -static -O2 -std=c11 -o target/testing/runtime/overlay/x86_64/coherence tests/runtime/overlay/coherence.c
```

The two executions used the corresponding command below concurrently, with a
different disposable `ROOT/etc` for each ISA:

```text
bwrap --unshare-user --uid 0 --gid 0 --unshare-pid --unshare-net \
  --unshare-ipc --unshare-uts --die-with-parent --ro-bind / / \
  --bind ROOT/etc /etc qemu-ISA target/testing/runtime/overlay/ISA/coherence
```

Both executions exited zero and produced byte-identical stdout with SHA-256
`531aea64a7029d760bbb2874d93c5e794938ea13962ba3eee5ce8a427a968b1b`.
For both ISAs, `hostname.moved` contained four bytes and `hostname.copy` had
mode 0644 and mtime 1700000001. This proves the ordinary Linux syscall and
shared-mapping shape on both guest ABIs.

It does **not** prove overlay copy-up or whiteout: the isolated native oracle
uses one writable filesystem view. The authoritative retained product harness
also inspects the upper tree for `.wh.hostname`, but the current YAML runner
cannot describe separate lower, upper, and work roots. Consequently both YAML
rows are typed `unsupported`; no native result is mislabeled as product-engine
success.

Generated binaries, namespace roots, and captures remain under
`target/testing/runtime/overlay`. This folder contains only its source, golden,
YAML definition, and audit evidence. No retained source or central legacy
ledger was modified.

## The runner can now describe lower, upper, and work

The `unsupported` reason above no longer holds. `TestImage` materializes through
`Images::materialize_overlay`, the case artifact is staged into the overlay
upper, and the spec is built from the durable rootfs reference, so the harness
takes the same `Service::rootfs_launch` overlay branch the product takes. The
`runtime/overlay/lower-*` rows exercise it.

Non-vacuity was established by mutation: with the spec rebuilt as
`ContainerSpec::from_directory(upper)` so the lower is absent, all five
`lower-*` rows fail at their first lower access (exit 1, 12, 20, 30, 40) and all
77 `runtime/filesystem` arm64 rows still pass. The corpus at large is therefore
insensitive to the lower by construction: 1674 of 1704 build-flag lines are
`-static`, so a corpus case is a self-contained binary staged into the upper
that resolves no name in the immutable chain.

## Whiteout publication is unwired

`runtime/overlay/lower-whiteout` fails on both ISAs at the point where a
lower-origin name must be removed: `unlink("/etc/passwd")` returns `EIO`.

The cause is direct: `path/overlay_publish.rs` defines `publish_whiteout` and
`publish_opaque` under `#[cfg(test)]`, and their only callers are that file's
own unit test. No production path publishes a whiteout or an opaque marker, so
no lower name can ever be hidden. Copy-up itself is wired and correct.

Owner: the `hl-engine` path resolver lane (STAT-FSTATAT).

## `coherence` fails for an unrelated reason: `/etc/hostname` is a name binding

The whiteout attribution above was wrong for `coherence`. That case never
reaches a lower-layer name at all. `Identity::mounts` binds `hosts`,
`resolv.conf` and `hostname` from the per-container service state directory onto
`/etc/*`, and `Service::launch_locked` applies them on every launch, so the
`/etc/hostname` the case opens is the runtime's own file, outside any layer.
The probe agrees: `rename("/etc/passwd")` succeeds with copy-up and a published
source whiteout, while `rename`/`unlink` of `hostname`, `hosts` and
`resolv.conf` return `EROFS` — exactly the three bound names.

Two independent defects sit behind exit 15, and only the first is a bug:

1. `path::mutation::prepare` returns `ReadOnly` for *any* name binding, ignoring
   the binding's `read_only` flag, while `path::host` honours it for `open`.
   `Identity::access` yields `ReadWrite` unless `read_only_root`, and
   `engine::spec` emits the `rw:` prefix, so all three identity bindings are
   writable — writable through `open`, read-only through every mutation. The
   case's `open(O_RDWR)`, `msync` and `ftruncate` therefore passed against the
   bound identity file, not against a copied-up lower.

2. Even if the rename were permitted the case would fail at exit 17. It asserts
   the moved file reads `MMca`, which requires the oracle fixture's `caca`
   contents; `Identity::prepare` writes `"{hostname}\n"`, and no harness path
   sets `spec.hostname`, so the file holds the 12-character container id.

Honouring `read_only` is therefore necessary but not sufficient, and it cannot
turn this row green. It is also not sufficient for safety: `unlink` and `rename`
must stay refused whatever the flag says, because the binding's parent is the
service state directory. `Identity::open`, used by exec and health, returns
`Corrupt` when one of the three names is missing, so a guest that could unlink
its own `/etc/hostname` would break exec and health for its container.

The engine oracle models these three paths differently again — as daemon-written
files in the overlay upper rather than binds — which is why its own
`tests/compat/overlay/coherence.c` can assert that the rename succeeds. Under a
binding model that assertion does not hold.
