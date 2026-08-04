# Linux syscall compatibility

This suite transfers every registered Linux-specific syscall guest from the
legacy `ext_linuxsys` group. The 43 original C files are retained byte-for-byte.
`manifest.tsv` records every registration, original path, ISA scope, build
contract, expected exit, dependency, and exact checked-in stdout.

Every active case runs through both production Linux guest engines. Each output
must byte-match its golden and the two engine outputs must byte-match each other.
No native oracle is used.

Two registrations require deterministic C adapters while retaining their exact
legacy sources: `epoll_reblock_fin_stable.c` removes the successful elapsed-time
measurement but preserves every failure diagnostic, and `pidfd_signal_stable.c`
lets the requested SIGTERM settle before the legacy cleanup SIGKILL. This turns
the old timing/race observations into stable behavioral contracts.
