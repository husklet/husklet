# QEMU evidence

On 2026-08-03 all seven active C fixtures were rebuilt from the migrated sources with their exact
manifest flags. Six matched the preserved exit and stdout contract under QEMU:

| Case | Target | Exit | Stdout |
|---|---|---:|---|
| `isa/aarch64 isa-regress` | arm64 | 0 | exact |
| `isa/x86_64 hello` | amd64 | 42 | exact |
| `isa/x86_64 ctest` | amd64 | 7 | exact zero bytes |
| `isa/x86_64 hx` | amd64 | 0 | exact |
| `isa/x86_64 glibc` | amd64 | 0 | exact |
| `isa/x86_64 glibc-min` | amd64 | 0 | exact |

The rebuilt `isa/x86_64 isa-regress` exited 0 but did not match stdout. The preserved golden SHA-256 is
`f5abb1461c02aa83c90610aa276d9c08b577f8279d76c9ea80165b868a7008af`; current QEMU produced
`83ddcb14f0248726289067a69d43dcd33ec41961cd7a096f8abf2518e6941507`. Differences begin in the
`subps-nan2`/`mulps-nan2` rows and continue through SSE3 horizontal NaN propagation: QEMU selects different
payload and sign bits than the retained engine contract. The golden was not rewritten. This case remains
visible as typed broken until the reference policy is resolved against an authoritative native x86 host.

The two Go cases are separately typed unsupported because the current folder builder cannot declare the
required `GOOS=linux GOARCH=amd64 CGO_ENABLED=0` environment per case. Unsupported rows are enumerated and
not passed to the C compiler.
