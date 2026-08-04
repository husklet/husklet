# Runtime compatibility tests

Each direct child directory is one removable test application. It owns its
source, image, build recipe, cases, optional oracle, fixtures, and golden bytes.
The runner discovers only direct child directories containing `test.yaml`; no
central registry or Makefile is involved.

```text
<name>/
  test.yaml
  main.c
  fixtures/       # optional, private to this application
  golden/         # exact expected stdout bytes
```

The YAML names the OCI image, guest artifact destination, per-ISA compilers,
compiler flags, oracle commands, and cases. The format is intentionally
unversioned: Husklet has no released compatibility contract for internal test
manifests, so the schema changes directly with the runner.

Run all applications for both architectures or select one application and ISA:

```text
cargo run -p testing -- runtime
cargo run -p testing -- runtime core --isa arm64
cargo run -p testing -- runtime core --isa amd64
```

Check committed golden bytes against the folder's reference emulator, or
replace them explicitly:

```text
cargo run -p testing -- oracle --check core --isa arm64
cargo run -p testing -- oracle --update core --isa arm64
```

Build products and image caches live under `target/testing`; application
folders contain no generated binaries. Removing an application directory
removes its complete definition without changing another test.

`legacy/` is the former monolithic CMake/Python corpus retained temporarily
while its cases are split into independent application folders. It is not the
new test API and receives no new cases.
