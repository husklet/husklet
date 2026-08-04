# Direct QEMU oracle evidence

On 2026-08-03 all 51 unique source files were compiled in parallel for both
production guest ISAs with the manifest flags (`-static -O2 -std=gnu11
-pthread -lrt`). This produced 102 ELF guests. Each guest was then run with a
five-second bound under `qemu-aarch64` or `qemu-x86_64`, and stdout was compared
byte-for-byte with its preserved golden. There were no timeouts: 81 executions
matched and 21 diverged.

Both ISAs diverged for `append_pwrite`, `bound_order`, `flag_einval`,
`iov_edges`, `mem_validate`, `read_badfd`, `read_eof`, `splice_edges`,
`timerfd_einval`, and `efault_syscalls`. AArch64 alone diverged for
`copyout_efault`. These are retained as visible broken or unsupported cases;
their expected bytes were not regenerated from the host/QEMU behavior.

All other unique source/golden pairs matched on both ISAs. The shared
`sentry_exec` source/golden serves both `sc-sentry-cloexec-exec` (with
`HL_UNTRUSTED=1`) and `sc-procfd-exec`; its direct no-engine oracle bytes match
on both ISAs, while the distinct environment and engine path remain separate
case contracts.

The repository-wide testing binary could not be used for this evidence because
an unrelated concurrent edit left `hl-images` calling the private method
`mirror_target`. The source builds and QEMU comparisons above are independent
of that Rust compilation failure.
