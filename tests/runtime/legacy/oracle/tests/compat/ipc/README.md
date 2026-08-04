# Linux IPC compatibility

This suite contains every C guest and every registered variant from the
legacy IPC matrix. Sources are preserved byte-for-byte. Each active case is
built as a static Linux executable for AArch64 and x86-64, run by its matching
production engine, compared byte-for-byte with `expected/shared`, and compared
across engines.

`manifest.tsv` is the authoritative contract. The suite is Linux-only, uses
checked-in expectations rather than a native oracle, and has no graphics, coordinator,
service, or display service dependency. The two former `untrusted` registrations map
to the same generic engine configuration because sandbox policy is no longer
an engine mode; they remain explicit active variants so registration coverage
is not lost.
