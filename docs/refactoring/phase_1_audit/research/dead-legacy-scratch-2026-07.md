# Dead/legacy scratch audit (2026-07)

This is a read-only classification of the assigned tracked-file scope. It does not authorize deletion by itself.

## Scope and machine-checkable coverage

The audit covered **40,657 / 40,657 tracked paths** (100%) and 2,685,149,621 bytes of uncompressed Git blobs.
The canonical sorted path list was produced with:

```sh
git ls-files -z -- scratch-erl scratch-t186 .slot_inspect.py .ckpt_stress 1.bin 1.map 'scratch-erl*' \
  | tr '\0' '\n' > audit.paths
wc -l audit.paths
sha256sum audit.paths
```

Expected result: `40657` lines and SHA-256
`7b28dc2a507e201917743e4d6930febcaeab6d9ae211a1a1184a62d15bb5824c`.

Every line is marked classified by exactly one disjoint rule below. These counts sum to 40,657:

| Classification rule | Count | Read/classified as | Finding |
|---|---:|---|---|
| `scratch-erl/rootfs/**` | 40,546 | captured Debian/Erlang container filesystem | remove candidate |
| `scratch-erl/*` (direct children only) | 91 | experiment scripts, copied engines and run/debug output | remove candidate, with scripts verified first |
| `scratch-t186/**` | 15 | translation-cache experiment and derived analysis files | remove candidate |
| `.ckpt_stress/**`, `.slot_inspect.py`, `1.bin`, `1.map` | 5 | unintegrated checkpoint/slot probes and generated traces | verify two checkpoint probes; otherwise remove |
| tracked paths beginning with a literal `"scratch-erl*` | 0 | requested odd-name check | none exist in current index |

Verification command for the partition:

```sh
awk '
  /^scratch-erl\/rootfs\// { a++; next }
  /^scratch-erl\//        { b++; next }
  /^scratch-t186\//       { c++; next }
  /^\.ckpt_stress\// || /^\.slot_inspect\.py$/ || /^1\.(bin|map)$/ { d++; next }
  { e++ }
  END { print a,b,c,d,e,a+b+c+d+e }
' audit.paths
# expected: 40546 91 15 5 0 40657
```

For all paths, the index path, blob type/size and reachability were inspected. Text scripts and logs were sampled
directly; binary/rootfs provenance was classified from file metadata, package database/manifests, symlink layout and
directory structure. A repository-wide reference search excluding the assigned trees returned no build, package,
CI, runtime or maintained-test consumers.

## Evidence-ranked findings

### Remove — high confidence

1. **Captured root filesystem: `scratch-erl/rootfs/**` (40,546 paths).** This is a complete mutable Debian container
   snapshot, not source: it contains `.dockerenv`, device nodes/symlinks, `/etc`, 37,384 `/usr` paths, 2,492 `/var`
   paths, package-manager state, Erlang runtime files, caches and `erl_crash.dump`. It occupies roughly 2.6 GB in the
   worktree. No Dockerfile, scenario, test, package rule or runtime path references it outside `scratch-erl`; only the
   adjacent one-off scripts use its absolute developer-machine path. If an Erlang regression remains valuable,
   replace this snapshot with a reproducible image/rootfs recipe and a small behavioral test fixture.

2. **Copied build products: `scratch-erl/ddjit-clean`, `scratch-erl/ddjit-diag2`,
   `scratch-erl/ddjit-diag3`, `scratch-erl/ddjit-fix`, `scratch-erl/ddjit-snap`,
   `scratch-erl/ddjit-snap2`, and `scratch-t186/ddjit-aarch64`.** These are checked-in arm64 Mach-O executables.
   They bypass the current build graph and are referenced only by sibling scratch scripts using absolute
   `/Users/x/dd/dd/...` paths. Rebuilding current engine targets is the only reliable reproduction.

3. **Generated logs/crash output: exact direct children matching `scratch-erl/*.log`, plus
   `scratch-erl/rootfs/erl_crash.dump`, `scratch-t186/java.out`, `scratch-t186/ns.out`, and
   `scratch-t186/dump.done`.** The 77 direct `.log` paths include `run_1.log` through `run_50.log`,
   `runf_1.log` through `runf_11.log`, `tl_1.log` through `tl_5.log`, `try_1.log`, `try_2.log`, and named
   diagnostic summaries. They are outputs of the adjacent scripts, contain machine-specific historical results, and
   have no consumers outside the scratch tree.

4. **Translation-cache captures and derived files: `1.bin`, `1.map`, `scratch-t186/_t.bin`,
   `scratch-t186/dump.bin`, `scratch-t186/dump.map`, `scratch-t186/dump.pc`, `scratch-t186/ns.bin`,
   `scratch-t186/ns.map`, `scratch-t186/region.bin`.** The Python tools parse fixed addresses such as
   `RWBASE=0x111aa8000` and overwrite derived binaries. No build or test target creates or consumes these paths.

5. **One-off Erlang/T186 experiment drivers: `scratch-erl/crashrep.sh`, `scratch-erl/diag2.sh`,
   `scratch-erl/diag3.sh`, `scratch-erl/hang_capture.sh`, `scratch-erl/hang_lldb.sh`,
   `scratch-erl/hang_lldb2.sh`, `scratch-erl/rep.sh`, `scratch-erl/rep_fork.sh`,
   `scratch-t186/analyze.py`, `scratch-t186/findbal.py`, `scratch-t186/findseq.py`, and
   `scratch-t186/follow.py`.** They are unreachable from maintained targets, depend on the captured binaries/rootfs,
   contain fixed process names, addresses or developer paths, and generate files already checked in beside them.

6. **Unintegrated slot probe: `.slot_inspect.py`.** It hard-codes
   `/Users/x/dd/dd/target-mac/release/ddcli`, workspace `slottest`, slot `insp`, and host storage paths. It has no
   caller and performs destructive process-group cleanup. Preserve the behavior only by converting it to a scoped
   Rust integration test with temporary workspace state.

### Verify before removal — medium confidence

1. **Checkpoint stress probes: `.ckpt_stress/stress.py` and `.ckpt_stress/closetime.py`.** They are not referenced by
   any target and hard-code `/Users/x`, `slottest`, `ddcli`, process matching and checkpoint paths. Unlike the captured
   artifacts, they encode potentially useful restore/Ctrl-C and concurrent-close behavioral journeys. Verify whether
   equivalent Rust coverage exists; if not, port the behavior into `dd-tests` before deleting these scripts.

2. **Historical environment flags in scratch-only scripts: `DDDBG_ENGFAULT` and `DD_HOSTNAME`.** Their only assigned-
   scope uses are the obsolete Erlang drivers above. Before calling either flag globally unused, search the engine and
   maintained tests separately; this audit establishes only that these scratch consumers do not justify retention.

### Keep — evidence does not support any assigned path

No assigned file is reachable from the current build, package, runtime, CI or maintained test graph. There is
therefore no high-confidence keep item. The two checkpoint scripts are verification candidates solely because their
behavior may merit migration, not because the files themselves are integrated.

## Reachability evidence

- `Cargo.toml`, all crate manifests, `Makefile`, `nix/**`, workflow/CI files, packaging files and maintained scripts
  contain no reference to any assigned path or directory.
- Repository-wide search excluding `scratch-erl/**`, `scratch-t186/**` and this audit finds no occurrence of
  `scratch-erl`, `scratch-t186`, `.slot_inspect`, `.ckpt_stress`, `1.bin`, or `1.map`.
- References inside the assigned scope are self-contained: Erlang shell scripts point to sibling copied `ddjit-*`
  binaries and `scratch-erl/rootfs`; T186 Python scripts read sibling dump/map files.
- The latest commit touching the assigned scope is `f26743cd` (`2026-07-09`, “wip(render): checkpoint before parallel
  rendering fine-tune”), which is not evidence of product reachability.

## Recommended cleanup order

1. Decide whether to port the two `.ckpt_stress` behaviors into Rust tests.
2. Remove generated logs, dumps, copied Mach-O engines and T186 captures.
3. Replace any still-needed Erlang reproduction with a recipe and small fixture, then remove the captured rootfs.
4. Add ignore rules for local crash dumps, cache dumps, rootfs exports and copied engine binaries so they cannot be
   recommitted accidentally.
