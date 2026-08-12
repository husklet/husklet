# Oracle C engine import plan

This historical sequencing plan was rooted at Husklet commit `719980785` and audits the read-only
oracle at `../engine` revision
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`. The oracle is not a build
dependency. Every imported file is copied into `retained/`, reviewed there,
and recorded in both retained source inventories.

Current status: the AArch64 interpreter tranche and complete x86 closure have
been imported. The x86 closure is compiled and inventoried but remains
production-unselected. Production has no Rust execution fallback; promotion of
another retained target must pass the gates below and update the fail-closed
selector explicitly.

[`ORACLE_IMPORT_MANIFEST.tsv`](ORACLE_IMPORT_MANIFEST.tsv) is the
source-by-source delta for the standalone core and both translator ISAs. Files
already present and byte-identical remain governed by
`retained/RUNTIME_SOURCES.manifest`. The five common translator files that
differ are explicit conflict rows; none may be overwritten wholesale.

## Authority boundary

The import restores C translation completeness, not oracle ownership of the
product. Rust remains authoritative for address-space ledgers, projection and
snapshot pins, mapping and instruction generations, executable tokens, cache
identity policy, dirty publication, signals, process lifecycle, checkpointing,
syscalls, container services, and application launch.

Consequently, oracle global logical-VMA pointers, identity-mapping fast paths,
mutation-time peer stopping, standalone configuration reads, and signal/process
singletons are not valid integration mechanisms. Imported code receives
bounded POD requests and returns bounded results through the retained ABI.

## Completed import tranches

The first tranche was the AArch64 interpreter pair:

- `translator/guest/aarch64/interp_dispatch.h`
- `translator/guest/aarch64/interp.c`

Both are now inventoried with a non-selected link smoke. Production continues
to use the same-ISA AArch64 translator rather than this interpreter arm.

The x86 backend was not safely divisible into individual lowering files: decoder,
operand, flags, emit, REP, vector, x87, cache, and generated dispatch headers
cross-include each other. The manifest's complete `x86_closure`, including
`core/target/{dual,x86_64}.c`, is now present. It remains non-selected pending
CPU snapshot, signal, syscall, dirty-publication, and differential tests.

The standalone CLI/config/environment/launch files are last and remain
unwired. Their purpose is a diagnostic standalone executable for oracle-floor
measurement. They must never become the application or daemon launch path.

## Conflict map

| Active source | Required treatment |
|---|---|
| `translator/cache.c` | Retain active checked windows and Rust-issued cache identity; port oracle lookup/eviction mechanics behind that identity. |
| `translator/guest/aarch64/cache.c` | Retain active generation and invalidation semantics; compare block lifetime and continuation behavior. |
| `translator/guest/aarch64/dispatch.h` | Retain the active Rust ABI layout and static assertions. |
| `translator/guest/aarch64/stubs.c` | Merge branch/IBTC mechanisms only; preserve active exit publication and diagnostics. |
| `translator/guest/aarch64/translate.c` | Merge lowering families only; preserve active snapshot guards, exact dirty journal, interrupts, and budget accounting. |

Additional x86 adaptations are named per row in the TSV. In particular,
`cpu.h` must match the Rust checkpoint codec, `operand.*` must use projections,
and `signal.c` cannot own delivery.

## Completion gates

Each tranche must satisfy, in order:

1. inventory parity and license checks;
2. archive compile and link-smoke without production selection;
3. ISA ABI/layout assertions and interpreter/native differential fixtures;
4. workspace library/bin tests and `hl-native` C execution tests;
5. deterministic product compatibility on both guest ISAs;
6. balanced, unique-ledger performance comparison against the pinned oracle;
7. same-binary control demonstrating that any measured win comes from the
   imported mechanism rather than layout or container lifecycle.

No tranche may change production selection until the next layer has independent
compatibility, non-vacuity, and performance evidence. There is no production
Rust fallback to preserve.
