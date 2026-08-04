# AArch64 failure histogram

The tables below preserve the post-ISA checkpoint used to choose the next
instruction cohort. The later concurrent production rerun after event and
filesystem routing was 153 pass / 597 fail / 377 skip. The later concurrent
deadline/readiness and fork rerun was 158 pass / 592 fail / 377 skip. The later
pipe/readiness rerun is 166 pass / 584 fail / 377 skip; consult
`report/api-results--isa-aarch64.tsv` for that current row-level evidence.
The later inotify checkpoint proves `syscall_edges/sc-shortread-inotify` on
both ISAs through the retained inventory runner; its full AArch64 combined
delta is recorded in `MIGRATION_HANDOFF.md` after the bounded rerun completes.
The measured `0x13800800` frontier is now decoded as the 32-bit `ROR` alias of
`EXTR`; the implementation covers general W/X two-register extraction and
reserved N/immediate encodings. Bounded `dentry-storm` and `forkwait` runs clear
that ISA boundary and expose later semantic gaps (`O_NOFOLLOW` errno behavior
and unrouted `wait4`, respectively).
The three durable `unsupported:0x5ef1bbff` rows were bounded-rerun as
`completeness/{cmp,int,tbl}` and still faulted first on that word. GNU identifies
it as scalar `ADDP Dd, Vn.2D`. The scalar-pair decoder now reuses the generic
wrapping horizontal-add reducer, rejects invalid Q/U/size forms, and preserves
PC, NZCV, and FPSR semantics across every source/destination alias combination.
All three retained cases now pass with jobs1 targeted inventory filters.

Generated from `report/api-results--isa-aarch64.tsv` by grouping fault rows on
reason and the first little-endian instruction word, and semantic rows on guest
status. Passing rows, explicit fixture skips, and worker/setup errors are
excluded. Ties are sorted by category for deterministic output.

## Before SSHL

| Count | Category |
| ---: | --- |
| 385 | `semantic:guest-status-0` |
| 73 | `semantic:guest-status-1` |
| 31 | `unsupported:0x4e224400` (`SSHL V0.16B, V0.16B, V2.16B`) |
| 17 | `semantic:guest-status-2` |
| 13 | `unsupported:0xd4207d00` |
| 9 | `memory:0xb9400260` |
| 5 | `unsupported:0x0f20a7ff` |
| 3 | `semantic:guest-status-10` |
| 3 | `semantic:guest-status-3` |
| 3 | `unsupported:0x93c88000` |
| 2 | `memory:0x885ffc40` |
| 2 | `semantic:guest-status-4` |
| 2 | `semantic:timeout` |
| 2 | `unsupported:0x5ef1bbff` |
| 2 | `unsupported:0x7effd400` |
| 2 | `unsupported:0xd50b7520` |
| 2 | `unsupported:0xda030085` |
| 1 | `decode:0x1e6343ff` |
| 1 | `decode:0x4ea1bbfe` |
| 1 | `decode:0x4ea1d820` |

Typed guest environment routing removed the four TZ worker-setup errors. All
eight cross-ISA TZ rows now enter guest execution. AArch64 `localtime-utc`,
`strftime-utc`, and `strptime` pass; `mktime-utc` reaches the measured
`0x93c40884` instruction gap. The x86-64 siblings likewise reach their current
instruction frontiers instead of failing before engine launch.

## Semantic classification

The report now records deterministic mismatch evidence without retaining guest
output: expected/actual FNV-1a hashes, lengths, first differing offset, and the
two bytes at that offset. Its companion `api-results--isa-aarch64.summary.tsv`
groups mismatch classes by suite.

After capability, pidfd, fstat, and shift-left-long routing, 407 failed rows end
with guest status zero.
Their exact output-mismatch classification is retained in the generated summary.
Before pidfd routing, the suite distribution was: completeness 66,
POSIX 63, syscall 55, filesystem 46, procfs 42, syscall edges 29, signals 28,
time 26, isolation 18, memory 13, process 11, and one each in libc and threads.
This is not one ISA cluster.

`completeness/capget` is a bounded representative. Its golden output is
`capget ok=1\n`; the actual output has the same 12-byte length and differs only
at offset 10 (`1` versus `0`). The final 16-call trace shows syscall 90
(`capget`) routed as unsupported and returning `ENOSYS` before the successful
write and exit. Typed capability routing now fixes that generic boundary: the
same persistent fixture receives a successful syscall and prints its exact
golden output. The full rerun confirms both AArch64 capget cases pass.

Grouping stdout failures by the exact tuple `(expected hash/length, actual
hash/length, first differing offset/bytes)` produces only singleton groups;
there is no repeated golden-output signature to optimize. Grouping the bounded
traces instead finds 40 rows containing startup `rseq` returning `ENOSYS`, but
those programs continue and this is not proof of their later output mismatch.
The task/descriptor pidfd lifecycle clears that causal cohort. All four
completeness rows (`pidfd-cap`, `pidfd-flags`, `pidfd-getfd`, and
`pidfd-signal`) now pass. The remaining pidfd failures are downstream and
distinct: the signals and syscall signal fixtures address a forked child that
is not yet represented by matching production task-registry identity, while
the syscall open fixture successfully opens the pidfd but its `fstat` reaches
the still-unrouted filesystem adapter. The skipped clone3 fixture still
requires the declared multi-process service.

## BRK classification

The two rows whose first fault word is `0xd4207d00` execute `BRK #0x3e8`.
This immediate is musl's intentional crash sentinel after an earlier failed
invariant; it is not an instruction that may continue. Generic clone/futex work
cleared eleven of the former thirteen rows. The remaining boundary is to trace
the preceding invariant per case while preserving BRK as a synchronous guest
fault.

## Current

The authoritative post-inotify/EXTR/scalar-ADDP jobs2 rerun contains 1,127 rows:
182 pass / 568 fail / 377 skip. This is a combined +16 pass delta from the
166/584/377 checkpoint; only separately targeted rows may be attributed to an
individual lane.

The leading current groups and their row attribution are:

| Count | Category | Rows |
| ---: | --- | --- |
| 386 | `semantic:guest-status-0` | Broad syscall/output mismatches; exact rows are retained in `api-results--isa-aarch64.tsv` |
| 63 | `semantic:guest-status-1` | Includes `completeness/pf-argv`, `filesystem/dentry-storm`, and filesystem policy cases |
| 51 | `semantic:timeout` | Includes DBT stress, blocked process, and memory reclamation fixtures |
| 20 | `semantic:guest-status-2` | Filesystem, memory aliasing, and page-size cases |
| 9 | `memory:0xb9400260` | `libc/{fgets-fputs,fpos,fread-fwrite,freopen,fscanf,fseek-ftell,getline,ungetc,ungetc-multi}` |
| 3 | `semantic:guest-status-10` | `syscall/{inotify-epoll,inotify-fdreserve}`, `threads/sentry-dup3-cleanup` |
| 2 | `memory:0x39000001` | `memory/{elf-rodata-fault,mapping-errors}` |
| 2 | `memory:0x885ffc40` | `filesystem/getdents-dtype`, `posix/readdir-dtype` |
| 2 | `unsupported:0x7effd400` | `libc/{atox,strto-float}` (`FABD D0,D0,D31`) |
| 2 | `unsupported:0xd50b7520` | `memory/{dbt-smc-bounce,mprotect-jit-interior}` (`IC IVAU,X0`) |
| 2 | `unsupported:0xda030085` | `libc/time-diff`, `time/calendar-roundtrip` (`SBC X5,X4,X3`) |
| 1 | `decode:0x1e6343ff` | `completeness/bf16` (`BFCVT H31,S31`) |
| 1 | `decode:0x4ea1bbfe` | `completeness/cvt` (`FCVTZS V30.4S,V31.4S`) |
| 1 | `decode:0x4ea1d820` | `completeness/neon-recip` (`FRECPE V0.4S,V1.4S`) |

The nine-word memory cluster is not a decode gap: `0xb9400260` is ordinary
`LDR W0,[X19]` and indicates common stdio state/lifetime failure. Excluding
semantic mismatches, timeouts, intentional sentinels, and downstream memory
symptoms, the largest genuine generic unsupported clusters tie at two. The next
selected cohort is `0xda030085` (`SBC`), because it is a generic integer
add-with-carry family shared by normal libc and calendar code rather than a
feature-specific vector or cache-maintenance operation.

That selected cohort is now implemented as the complete W/X
`ADC`/`ADCS`/`SBC`/`SBCS` family. The interpreter preserves carry-in and
not-borrow semantics, computes architectural N/Z/C/V only for flag-setting
forms, handles ZR sources and discarded ZR destinations, and advances PC.
Exhaustive tests cover 768 width/operation/flag/carry/value/destination
combinations plus ZR forms. Bounded jobs1 reruns now pass for both attributed
rows: `libc/time-diff` and `time/calendar-roundtrip`.

The tied `0xd50b7520` cohort is also resolved. `IC IVAU, Xt` now crosses a
generic instruction-cache invalidation port; the current interpreter refetches
guest instructions on every step, while the port preserves the correct
boundary for a future translated-code cache. Tests cover all 32 source
register encodings, including XZR, and verify PC and NZCV behavior. Bounded
jobs1 reruns pass for both attributed rows: `memory/dbt-smc-bounce` and
`memory/mprotect-jit-interior`.

The remaining `0x7effd400` scalar `FABD` cluster is decoded for both S and D
formats and evaluated through deterministic soft-float subtraction followed by
architectural absolute-value selection. `libc/atox` now passes. The
`libc/strto-float` row progresses beyond `FABD` to `0x7f6007fe` (`USHR
D30,D31,#32`), so the measured `FABD` category is gone even though that longer
fixture still has a subsequent migration boundary.

The subsequent `0x7f6007fe` scalar `USHR D30,D31,#32` boundary is now covered
as the complete `USHR Dd,Dn,#imm` family. Tests exhaust all 65,536 legal
immediate/source/destination encodings, reject the reserved immediate space,
and verify shifts 1 through 64, aliases, scalar upper-lane clearing, PC advance,
and unchanged NZCV/FPSR. The exact jobs1 `libc/strto-float` rerun advances to
`0x1e601fe0` (`FCSEL D0,D31,D0,NE`); the fixture is not yet an end-to-end pass.

Scalar `FCSEL` now covers S/D formats, every condition code, and every source,
alternate, and destination register encoding. Selection copies the chosen raw
IEEE bits, including signaling/quiet NaNs and signed zero, without arithmetic,
canonicalization, or FPSR mutation; scalar writes clear the upper vector bits.
The exact jobs1 `libc/strto-float` rerun now passes end to end.

After generic SSHL and shift-left-long support, the `0x4e224400` and
`0x0f20a7ff` clusters are absent. The six shift-left-long rows now terminate as
semantic mismatches rather than ISA faults:

| Count | Category |
| ---: | --- |
| 407 | `semantic:guest-status-0` |
| 83 | `semantic:guest-status-1` |
| 20 | `semantic:guest-status-2` |
| 9 | `memory:0xb9400260` |
| 4 | `semantic:guest-status-10` |
| 3 | `semantic:guest-status-4` |
| 3 | `unsupported:0x5ef1bbff` |
| 3 | `unsupported:0x93c88000` |
| 2 | `semantic:guest-status-3` |
| 2 | `memory:0x39000001` |
| 2 | `memory:0x885ffc40` |
| 2 | `unsupported:0x7effd400` |
| 2 | `unsupported:0x93c40884` |
| 2 | `unsupported:0xd4207d00` |
| 2 | `unsupported:0xd50b7520` |
| 2 | `unsupported:0xda030085` |
| 1 | `semantic:guest-status-11` |
| 1 | `decode:0x1e6343ff` |
| 1 | `decode:0x4ea1bbfe` |
| 1 | `decode:0x4ea1d820` |
| 1 | `memory:0xb900001f` |
| 1 | `memory:0xb9400004` |
| 1 | `memory:0xb9400040` |
| 1 | `memory:0xf9000002` |
| 1 | `unsupported:0x0d4006bf` |
| 1 | `unsupported:0x13800800` |
| 1 | `unsupported:0x1ac54020` |
| 1 | `unsupported:0x4e284b9f` |
| 1 | `unsupported:0x4e3ecffc` |
| 1 | `unsupported:0x4e9da7c1` |
| 1 | `unsupported:0x4f4e83fe` |
| 1 | `unsupported:0x4f9fe3fe` |
| 1 | `unsupported:0x5e0b07fe` |
| 1 | `unsupported:0x5e1a639f` |
| 1 | `unsupported:0x5e1c03db` |
| 1 | `unsupported:0x5ef1bb9c` |
| 1 | `unsupported:0x6e30fbff` |
| 1 | `unsupported:0x93c05c02` |
| 1 | `unsupported:0x93c2cc42` |
| 1 | `unsupported:0x9a090021` |
