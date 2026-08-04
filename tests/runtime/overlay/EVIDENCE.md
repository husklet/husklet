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
