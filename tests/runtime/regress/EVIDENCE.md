# Regression cohort evidence

The authoritative retained registrations are the `core/regress` rows in
`../engine/tests/compat/core/regress/manifest.tsv` and the legacy inventory,
build-plan, and fixture-schema ledgers. This folder contains fifteen logical
cases and twenty-three ISA rows. Every fixture contract is a plain executable:
there are no side files, rootfs trees, symlinks, special devices, or external
network setup. Every row has a 120-second bound and an empty environment.

Sources and stdout goldens were copied byte-for-byte. File names were changed
only to satisfy the repository filename policy; hashes are compared against the
retained paths during review.

`go-cgo-sigurg` is retained as source plus golden and is explicitly unsupported:
the YAML runtime builder selects one C compiler per target and cannot express
the required `CGO_ENABLED=1`, external static Go link. Keeping the historical
ELF would create forbidden prebuilt source state. The case becomes active only
after the typed builder can describe and reproduce that toolchain operation.

## Bounded verification

On 2026-08-04 the repository runner used the pinned per-target compilers and
QEMU commands with `--jobs 18`. All twenty-two C/ISA rows compiled with their
retained flags. Twenty active QEMU rows matched the retained stdout and exit
status byte-for-byte.

`nonpie-guest-ptrs` compiled for both ISAs and exited zero under both QEMU
commands, but QEMU printed `set_robust_list=BAD`; every other output token
matched. The retained engine golden says `set_robust_list=ok`. The two rows are
therefore typed broken rather than silently replacing an engine-specific
contract with QEMU output. The exact commands were:

```text
HL_COMPAT_JOBS=18 testing oracle regress --check --jobs 18
HL_COMPAT_JOBS=18 testing oracle regress --check --isa amd64 --jobs 18
```

The Go/cgo row was not compiled because no Go compiler is present and the
typed builder cannot encode its required toolchain environment. No prebuilt,
result, or generated artifact was added beneath this folder.
