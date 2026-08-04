# Signal compatibility suite

This suite preserves all 12 guests registered by the legacy `ext/signalx.rs` group. Sources are byte-identical transfers. The manifest records the complete registration contract; former native-oracle cases use deterministic normalized checked-in goldens, so validation needs no native oracle.

Every active case is compiled statically for Linux AArch64 and x86-64, run through the production engines under the ABI4 matrix runner, and checked for exact golden output and cross-engine equality. Timing and asynchronous-delivery cases also require repeated soak runs.
