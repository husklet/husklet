# Repository compatibility scenarios

Each direct child directory owns one `test.yaml` definition and its local source
and golden files. The repository testing application discovers those definitions
without a Rust registry or category wrapper.

Run the quick suite with:

```text
nix develop --command cargo run -p testing --bin testing -- scenarios --class quick
```

List one scenario without materializing images:

```text
nix develop --command cargo run -p testing --bin testing -- scenarios languages --list
```
