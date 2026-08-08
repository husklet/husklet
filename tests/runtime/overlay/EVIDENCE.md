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

## Whiteout publication was unwired, and is now wired

`path/overlay_publish.rs` defined `publish_whiteout` and `publish_opaque` under
`#[cfg(test)]`, with that file's own unit test as their only caller, so no
production path ever hid a lower name. `unlink("/etc/passwd")` returned `EIO`.

The `EIO` was `EBADF`: `ParentLease::as_raw_fd` yields `-1` when the walk
selected a lower parent and no upper copy exists, and `HostError` maps an
unlisted errno to `Io`. Three separate operations reached the host that way —
`unlinkat`, and also `mkdirat`/`symlinkat` under a lower-only parent, neither of
which had anything to do with markers.

What now publishes a marker, and when:

* `overlay_publish::remove` runs for every `Unlink`. It publishes `.wh.NAME` in
  the upper only when a lower still provides the name; an upper-only name keeps
  the kernel's own `unlinkat` so its errno reaches the guest unchanged. The
  merged view decides the type (`EISDIR`/`ENOTDIR`) and, for `rmdir`, the
  emptiness (`ENOTEMPTY`), so no lower name is masked on a request Linux would
  have refused.
* `Rename` copies a lower-backed source up, moves it, then whites out the source.
* `Directory` publishes `.wh..wh..opq` inside a directory recreated over a name a
  lower still provides as a directory, so its stale children stay hidden.
* Every create clears a stranded marker only *after* its host call succeeds, so
  a refused create cannot resurrect the name the marker was hiding.

A subsequent lookup sees the marker because `pin::paths::whiteout_at` probes the
upper before the lower candidates and returns `NotFound`; `readdir` sees it
because `directory::overlay_layer` already parsed `.wh.` and
`overlay_entries::merge` already suppressed it; copy-up sees it because
`CopyUp::commit` already cleared it. Only the write side was missing. Markers
live exclusively in the writable upper, so the committed-chain invariant the
layer name index proves is untouched.

`sync_directory` was also fixed: it fsynced the pinned parent, which is normally
an `O_PATH` capability, and `fsync` refuses those with `EBADF`. It now fsyncs a
readable reopen. This was latent in `CopyUp::commit` too and only surfaced once a
second marker was published into an already-existing upper directory.

### Non-vacuity by mutation

Suppressing the marker rename in `publish_whiteout` fails exactly the two rows
that assert deletion, at exactly the assertion that a deleted name is still
present: `lower-whiteout` exit 31 (`stat("/etc/passwd")` still succeeds) and
`lower-whiteout-dir` exit 54 (`stat("/srv")` still succeeds). The other four
overlay rows are unaffected. Suppressing only the opaque marker fails only
`lower-whiteout-dir`, at exit 71, where the recreated `/media` re-lists the
lower's children.

## `coherence` is a name-binding defect, not a whiteout defect

The earlier attribution of `coherence` to whiteout publication was wrong, and its
description of what the case proves was wrong too. A probe built from the same
overlay root reports:

```text
rename /etc/passwd -> 0 errno=0      old passwd stat=-1 errno=2 (ENOENT)
rename /etc/hostname -> -1 errno=30  (EROFS)
rename /etc/hosts    -> -1 errno=30  (EROFS)
unlink /etc/hostname -> -1 errno=30  (EROFS)
```

A lower-only name in the same directory renames correctly, with its copy-up and
its source whiteout both published — so the machinery `coherence` was blamed on
demonstrably works. The three names that fail are exactly `hosts`,
`resolv.conf` and `hostname`, the per-container identity files that
`hl-container`'s `Identity::mounts` binds into `/etc`.

`path::mutation::prepare` computes `projected` from
`Source::name_binding(..).is_some()` and returns `ReadOnly` for any name binding,
ignoring the `read_only` flag the binding carries. `path::host` gets this right
for `open`: it refuses only when `binding.read_only`, and otherwise opens the
bound host file through `binding.parent`/`binding.leaf`. `coherence`'s
`open("/etc/hostname", O_RDWR)` therefore never touched the lower at all — its
copy-up, `msync` and `ftruncate` assertions passed against the bound identity
file, not against a layer.

Making the row pass needs mutations on a read-write name binding to be honoured
and routed through the binding, the way `open` already routes them. Renaming the
layer entry instead would only move the failure from exit 15 to exit 17, and
allowing the operand through unrouted would let a guest `rename` or `unlink` the
container's identity files out of the service state directory. That is a
name-binding change, not an overlay one, so the row stays typed with the correct
reason rather than being made green.

Owner: the `hl-engine` path resolver lane (STAT-FSTATAT); the remaining
`coherence` obligation belongs with name bindings.
