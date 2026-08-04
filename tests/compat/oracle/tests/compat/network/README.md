# Network compatibility

This suite preserves all 29 C guests from `hl-jit-darwin/testdata/guests/ext_net` byte for byte. The manifest preserves every Rust engine-matrix registration, including both guest ISAs and the explicit environment for `lo-any-bridge`.

`make compat-network` compiles every guest for Linux AArch64 and Linux x86-64, runs each binary through its corresponding production engine, checks exact checked-in stdout and exit status, and checks cross-engine equality. No native oracle or source-text inspection is used.
