# Socket-stop status

The preserved source creates a socketpair and blocks in `read` without a peer
writer. Its legacy contract depended on external stop/cancellation orchestration
that the folder runtime runner does not yet model. Direct QEMU execution is
therefore expected to time out rather than produce a standalone exit result.
