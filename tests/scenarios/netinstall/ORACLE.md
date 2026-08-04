# Network installation oracle

These end-to-end cases preserve the commands, images, timeouts, and expected
output from `tests/scenarios/fixtures/netinstall-core.yaml`. They exercise
package repository access and applications installed inside ordinary OCI
images through the integrated Rust container engine.

This migration changes only repository test ownership and representation. It
does not change a runtime domain, so no retained C implementation was used as
an implementation oracle and `/Users/x/dd/engine` was not modified. The
checked-in golden files are the existing scenario expectations, not generated
approximations.
