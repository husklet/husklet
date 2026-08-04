# Network installation oracle

These six end-to-end cases preserve the commands, images, timeouts, and
expected output formerly declared in
`tests/scenarios/fixtures/netinstall-core.yaml`. They exercise package
repository access and applications installed inside ordinary OCI images
through the integrated Rust container engine.

`test.yaml` is the sole declarative registration for this category. Every
stable ID, image, command, target, class, timeout, and output value matches the
retired fixture. The directory-local definitions additionally declare the
actual network, registry, disk, and process resource bounds instead of relying
on the legacy network task's coarse `host_port` fallback.

This migration changes only repository test ownership and representation. It
does not change a runtime domain, so no retained C implementation was used as
an implementation oracle and `/Users/x/dd/engine` was not modified. The
checked-in golden files are the existing scenario expectations, not generated
approximations.
