# QEMU oracle evidence

All sixteen declared cross-builds succeeded. `namespace_boundary`,
`inet_loopback`, and `prctl_lifecycle` matched their goldens on both ISAs.
`uname_boundary` exited 4 with empty stdout; `inet_isolated`, credential
mutation, and both seccomp cases exited 1 with differing stdout on both ISAs.
These provider/host-policy divergences remain typed broken and visible.
