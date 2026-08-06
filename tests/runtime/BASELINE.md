# Native compatibility corpus baseline at `3debb5997`

This is a read-only execution report, not a claim that the compatibility
domain is complete.  The source tree was the clean detached commit
`3debb5997c7839588b2d4a922ca565ef24845dc9`, tree
`91467d76ac060aa55eef8bbecd46283f06ea7380`.  The report was recorded on a
Linux host with 18 logical CPUs.  Raw ledgers remain bounded run artifacts and
are not checked into the repository.

## Native-mode proof

The proof row was selected through the typed engine-option channel, never by
putting engine options directly in the supervisor environment:

```sh
HL_TEST_ENGINE_APP_BIN_DIR=/var/tmp/legacy-parity-v2.pBrGzZ/target/release \
HL_COMPAT_ENGINE_OPTIONS='HL_NATIVE_EXECUTION=1;HL_NATIVE_DIAGNOSTICS=1' \
HL_COMPAT_JOBS=1 \
/var/tmp/legacy-parity-v2.pBrGzZ/target/release/testing runtime abi \
  --case runtime/abi-alloca --isa amd64 --jobs 1 \
  --results target/corpus/native-proof-active.tsv
```

The row passed in 1,342 ms.  Its diagnostic was
`hl-native: runs=4 builds=141 hits=1840 fallbacks=0 sites=0 services=16`;
therefore this is execution evidence, unlike a loader-only row with zero runs
and builds.  The proof ledger SHA-256 is
`a74941b7bef00e9f452181d84401a253e4172d13951264bb0e5f6f4c332c066f`.

## Corpus result

The initial resumable command was:

```sh
HL_TEST_ENGINE_APP_BIN_DIR=/var/tmp/legacy-parity-v2.pBrGzZ/target/release \
HL_COMPAT_ENGINE_OPTIONS='HL_NATIVE_EXECUTION=1;HL_NATIVE_DIAGNOSTICS=1' \
HL_COMPAT_JOBS=18 HL_COMPAT_RESUME=1 \
/var/tmp/legacy-parity-v2.pBrGzZ/target/release/testing runtime \
  --jobs 18 --resume --results target/corpus/full.tsv
```

The 18-worker run had durably recorded 916 unique rows (907 pass, 9 fail) when
it first stopped at the ledger byte bound.  A one-worker resumability probe
committed one preceding row, leaving the final partial artifact at exactly 917
unique rows (908 pass, 9 fail), before reproducing the same bound at the next
sorted key.  The remaining non-soak applications were then run with
the same options and one result path per application.  A deterministic
last-report-wins merge, sorted by the exact `(case ID, target)` key, produced:

| Result | Rows |
|---|---:|
| pass | 2,657 |
| fail | 165 |
| executed | 2,822 |
| eligible | 2,845 |
| blocked | 23 |

The merged ledger was
`/var/tmp/legacy-parity-v2.pBrGzZ/merged.tsv`, SHA-256
`45c41da46c212ade2479fb3de8e19ab5a74b521a2dbdb0264a991229821af68b`.
All 2,822 `(case ID, target)` keys were unique.  The initial partial ledger was
`target/corpus/full.partial.tsv`, SHA-256
`fcfd099ee7030fb23ff2289dcb6510f8d50bed9fd7e49808bec47c19276f536d`.
These values were verified with `sha256sum` after every worker and supervisor
had exited.  `git rev-parse HEAD HEAD^{tree}` in the clean execution worktree
returned the commit and tree stated above; `git status --short` was empty.

The blocked row is exactly `runtime/forkpipe amd64`.  A focused one-worker run
completed guest execution in about five seconds but failed with
`testing: runtime result row exceeds its byte bound`.  Resumption retries that
same sorted key.  The command surface cannot exclude one case while running
the remainder of its application, so the other 22 unrecorded soak rows are
reported as blocked rather than silently omitted.  No capture bound was raised
and no diagnostic was truncated to manufacture a result.

## Largest observed failure domain

The 112 network failures are 56 logical cases, each failing on both ARM64 and
AMD64.  Fifty-five pairs have byte-identical diagnostics across ISAs;
`netlink-edges` differs only in the detailed current interface observations.
This makes the cohort predominantly common-root networking evidence, not 112
independent ISA bugs.

| Classification | Cases | ISA rows | Representative symptom |
|---|---:|---:|---|
| specific socket contract after a failed/empty endpoint | 21 | 42 | empty accept/read payload, zero socket metadata, or wrong readiness |
| socket operation returns zero/failure | 13 | 26 | bind/connect/listen/transfer or sockopt success remains zero |
| fixture exits nonzero before a detailed verdict | 13 | 26 | exit code 1 on both ISAs |
| invalid common socket root | 5 | 10 | repeated `EBADF` from unrelated socket operations |
| capability unavailable | 4 | 8 | `ENOSYS` socket creation or `no_ipv6` |

Successful network rows are concentrated in address conversion and Unix
socket behavior (`socketpair`, Unix stream/datagram/seqpacket, credentials,
`SCM_RIGHTS`, and generic message-vector operations).  The failures instead
concentrate in INET/INET6 endpoint construction, host interface visibility,
TCP/UDP bind/connect/listen/data flow, and socket options.  The next audit
should therefore begin at the shared INET provider/fixture construction and
endpoint lifecycle, then split only mechanisms that still fail after that
common root is established.

## Runner provenance and resource bounds

The retained orchestration studied was
`../engine/tools/matrix_runner.c::{load_manifest,isa_servable,run_one,main}`,
`../engine/tools/compat_runner.c::{run_one,main}`, and
`../engine/cmake/Phase3Compat.cmake::hl_compat_suite`, including the
`hl_guest_binary`/`hl_guest_suite` registration path.  The current typed owners
were `src/apps/testing/src/runtime/definition.rs` for YAML validation and
selection, `runtime/execution.rs` plus `execution/worker.rs` for bounded worker
lifecycle, and `runtime/ledger.rs` for synchronized resumable rows.

The wide lane used all 18 logical CPUs.  Observed available RAM stayed at
15--17 GiB, free swap stayed at 20 GiB, and free run-artifact disk stayed above
112 GiB.  The unique release target was 1.1 GiB and result ledgers were under
300 KiB.  Workers were bounded by manifest deadlines (30--240 seconds), no
escaped worker remained, and the final zombie count was zero.  The run was
limited by guest execution and process startup, with the final coverage gap
caused solely by the durable row-size bound.
