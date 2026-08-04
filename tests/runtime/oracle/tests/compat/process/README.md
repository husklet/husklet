# Process compatibility corpus

This directory is the complete fourteen-case `ext_proc` corpus. Every C source is byte-identical to its
legacy original, and the manifest records the full `processx.rs` registration contract. Former native-oracle
cases now use reviewed, deterministic Linux semantic goldens; the obsolete qemu-x86 clone3 xfail is not part
of the production contract.

The generic matrix builds static AArch64 and x86_64 Linux guests, with the required `-no-pie` build retained
for `nonpie_ptrargs.c`, launches both production engines through ABI4 configs, checks exact exit/stdout, and
requires cross-ISA equality. Fork, futex, ptrace, spawn/exec, wait, and signal lifecycle cases are soaked.
The nested `procexe/selfexe.c` adds the exact `/proc/self/exe`, readlink/readlinkat, execve/execveat, and
Linux comm behavior registrations in default and `comm` modes, using reviewed deterministic goldens.
