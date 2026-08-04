# Socket-stop migration evidence

This byte-exact seed deliberately blocks reading an idle Unix stream pair. Its
legacy contract was an external engine stop followed by cancellation, endpoint
closure, and process reap. The unified runner cannot yet express an expected
external-stop transition, so the case remains visibly broken instead of being
misrepresented as an ordinary exit or timeout.

## Whole-category migration evidence

The legacy IPC manifest's 126 registrations were moved into this one canonical
folder without changing source or expected-output bytes. Together with the three
existing registrations, `test.yaml` contains 129 stable `runtime/ipc/<case>`
IDs: 119 active and 10 explicitly broken. The 124 unique imported sources plus
the three existing sources are paired with 127 goldens. Two legacy untrusted
variants intentionally reuse their generic source and golden while retaining
distinct case IDs. Every source and golden filename has at most two semantic
underscore-separated words.

On 2026-08-03, `testing oracle ipc` ran independently for AArch64 and x86-64
with `--jobs 18`. It built each source directly with its registered compiler and
flags, executed the matching QEMU provider, and checked exit status and stdout
byte-for-byte. All 235 active case/ISA registrations passed. The run started
with 17 GiB available RAM, 26 GiB free swap and 209 GiB free disk; concurrency
was CPU-bound and left no escaped descendants. QEMU divergences for
`scm-rights-trunc` and `scm-epoll` were recorded as typed broken cases rather
than debugged fixture-by-fixture or used to rewrite retained goldens.
