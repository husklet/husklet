# Positional I/O evidence

Both source-built static ELFs reach the fixture's initial
`openat("/positional")` under QEMU, but the unprivileged host oracle cannot
create that absolute guest path. Both ISA rows remain visible and typed broken
pending an oracle root that maps guest `/` independently from the host.

The path-metadata source was also compiled for both ISAs and executed in a
fresh temporary working directory. Both QEMU runs exited zero with exact
zero-byte stdout. The shared oracle runner currently executes from the
repository root rather than an isolated scratch directory, where pre-existing
names make the fixture's cleanup assertions fail. Its row remains typed broken
until the runner supplies the already-proven isolated working directory.
