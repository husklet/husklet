# `src/translator/host/x86_64/`

One file: `asm.h`, the x86-64 instruction assembler. It is the exact sibling of `../aarch64/asm.{c,h}` — the
per-host-CPU **assembler**, not a per-host-CPU code generator — and it exists because there is now a second
host CPU that emits machine code.

This directory was empty until then, and the reason it was empty is still the reason it holds only this. It
used to hold `codegen.c` + `x86_64_codegen.h`, a lowerer for the IR in `include/hl/ir.h`. That IR, its lowerers,
and the public `hl/ir.h` + `hl/codegen.h` headers were deleted: `hl_codegen_*` had no caller anywhere in `src/`,
and the 17-opcode IR could express neither production frontend (no flags, no vectors, no atomics, no syscalls).
A symmetric `host/<cpu>/codegen.c` per host CPU reads exactly like the production lowering pipeline, and it is
not one: the engine's guest frontends under `src/translator/guest/` emit host machine code **directly**.

`asm.h`'s only consumer is `../../guest/x86_64/translit/`, the same-ISA transliterator.
It encodes the fixed vocabulary that transliterator needs at block boundaries
— `%gs`-relative loads and stores, `movabs`, `pushfq`, `jcc rel32`, the guest-stack accesses a `CALL`/`RET`
performs. It does **not** encode guest instructions: those are copied verbatim, which is the whole point of the
diagonal.
