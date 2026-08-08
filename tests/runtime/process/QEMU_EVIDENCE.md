# QEMU oracle evidence

All sixteen declared cross-builds succeeded. `namespace_boundary`,
`inet_loopback`, and `prctl_lifecycle` matched their goldens on both ISAs.
`uname_boundary` exited 4 with empty stdout; `inet_isolated`, credential
mutation, and both seccomp cases exited 1 with differing stdout on both ISAs.
These provider/host-policy divergences remain typed broken and visible.

`inet_isolated` is oracle-excluded rather than broken: QEMU user mode shares the
host network namespace, so it can never referee an isolated-namespace routing
contract. Its former golden required `socket(AF_INET)` and `socket(AF_INET6)` to
fail `ENOSYS` under `HL_NET_ISOLATE`. That contract was stale in two ways.
Linux never answers `socket()` with `ENOSYS` for a blocked family; it answers
`EAFNOSUPPORT`. And an isolated network namespace is loopback-only, not
socket-less. Run on this host, `unshare -rn` with `lo` brought up -- the same
shape as Docker `--network none`, and the same shape the engine models with
`lo` flags `0x49` -- creates all three sockets, refuses a connect to an external
IPv4 or IPv6 address with `ENETUNREACH`, and admits a loopback connect. The
engine's loopback-only stack was therefore correct and the golden was wrong.

Re-recording the golden alone would have dropped the guarantee the option
exists to provide, so the fixture now asserts the routing contract as well.
Doing so exposed a real hole: `RuntimeNetworkSyscalls::connect` consulted
`route()` only after the datagram fast path returned, so under `HL_NET_ISOLATE`
a UDP `connect()` to 8.8.8.8 succeeded while the stream equivalent was correctly
refused. The route check now precedes the transport split.
